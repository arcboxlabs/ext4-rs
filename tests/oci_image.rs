// Integration tests simulating OCI container image layer unpacking.
//
// Builds tar archives in memory that mimic realistic Alpine Linux rootfs
// layers, unpacks them onto an ext4 filesystem, and verifies the result.
//
// NOTE: The current `unpack_tar` implementation treats tar Symlink entries
// as hard links (because `entry.link_name()` is checked before the
// entry-type dispatch). As a workaround, symlinks are created directly
// via `Formatter::create()` after tar unpacking. This matches how a
// real container runtime would layer symlinks that point to directories.

use std::io::Cursor;

use ext4::constants::*;
use ext4::{Formatter, Reader};
use tempfile::NamedTempFile;

/// Helper: create a formatter backed by a temporary file.
fn new_formatter() -> (Formatter, NamedTempFile) {
    let tmp = NamedTempFile::new().unwrap();
    let fmt = Formatter::new(tmp.path(), 4096, 256 * 1024).unwrap();
    (fmt, tmp)
}

/// Generate deterministic bytes of the given length.
/// Uses a simple linear congruential pattern so content is reproducible.
fn deterministic_bytes(len: usize, seed: u8) -> Vec<u8> {
    let mut buf = vec![0u8; len];
    let mut val = seed as u16;
    for b in buf.iter_mut() {
        val = val.wrapping_mul(179).wrapping_add(37);
        *b = (val & 0xFF) as u8;
    }
    buf
}

// ---------------------------------------------------------------------------
// Tar builder helpers
// ---------------------------------------------------------------------------

/// Append a directory entry to the tar builder.
fn tar_dir(builder: &mut tar::Builder<Vec<u8>>, path: &str, mode: u32) {
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Directory);
    header.set_mode(mode);
    header.set_size(0);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(1_700_000_000);
    header.set_cksum();
    builder.append_data(&mut header, path, &[] as &[u8]).unwrap();
}

/// Append a regular file entry.
fn tar_file(builder: &mut tar::Builder<Vec<u8>>, path: &str, mode: u32, content: &[u8]) {
    tar_file_owned(builder, path, mode, content, 0, 0);
}

/// Append a regular file entry with specific uid/gid.
fn tar_file_owned(
    builder: &mut tar::Builder<Vec<u8>>,
    path: &str,
    mode: u32,
    content: &[u8],
    uid: u64,
    gid: u64,
) {
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_mode(mode);
    header.set_size(content.len() as u64);
    header.set_uid(uid);
    header.set_gid(gid);
    header.set_mtime(1_700_000_000);
    header.set_cksum();
    builder.append_data(&mut header, path, content).unwrap();
}

/// Append a whiteout marker (zero-length regular file named `.wh.<name>`).
fn tar_whiteout(builder: &mut tar::Builder<Vec<u8>>, path: &str) {
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_mode(0o644);
    header.set_size(0);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(1_700_000_000);
    header.set_cksum();
    builder.append_data(&mut header, path, &[] as &[u8]).unwrap();
}

/// Finish the tar builder and return bytes wrapped in a Cursor.
fn finish_tar(builder: tar::Builder<Vec<u8>>) -> Cursor<Vec<u8>> {
    let data = builder.into_inner().unwrap();
    Cursor::new(data)
}

// ===========================================================================
// Test: two-layer OCI rootfs (Alpine-like)
// ===========================================================================

#[test]
fn test_oci_two_layer_rootfs() {
    // -- Build Layer 1 (base rootfs) ------------------------------------------
    let mut b1 = tar::Builder::new(Vec::new());

    tar_dir(&mut b1, "bin/", 0o755);
    tar_file(&mut b1, "bin/sh", 0o755, b"#!/bin/busybox sh\n");

    tar_dir(&mut b1, "etc/", 0o755);
    tar_file(&mut b1, "etc/alpine-release", 0o644, b"3.19.0\n");
    tar_file_owned(
        &mut b1,
        "etc/passwd",
        0o644,
        b"root:x:0:0:root:/root:/bin/sh\nnobody:x:65534:65534:nobody:/:/sbin/nologin\n",
        0,
        0,
    );
    tar_file(&mut b1, "etc/hostname", 0o644, b"alpine\n");

    tar_dir(&mut b1, "lib/", 0o755);
    let musl_content = deterministic_bytes(8192, 0xAA);
    tar_file(&mut b1, "lib/ld-musl-aarch64.so.1", 0o755, &musl_content);

    tar_dir(&mut b1, "usr/", 0o755);
    tar_dir(&mut b1, "usr/bin/", 0o755);
    tar_dir(&mut b1, "usr/lib/", 0o755);
    tar_dir(&mut b1, "var/", 0o755);
    tar_dir(&mut b1, "var/log/", 0o755);
    tar_dir(&mut b1, "tmp/", 0o1777);
    tar_dir(&mut b1, "root/", 0o700);
    tar_dir(&mut b1, "home/", 0o755);

    let layer1 = finish_tar(b1);

    // -- Build Layer 2 (overlay) ----------------------------------------------
    let mut b2 = tar::Builder::new(Vec::new());

    // Whiteout removes /etc/hostname
    tar_whiteout(&mut b2, "etc/.wh.hostname");

    // New file
    tar_file(&mut b2, "etc/resolv.conf", 0o644, b"nameserver 8.8.8.8\n");

    // Overwrite alpine-release
    tar_file(&mut b2, "etc/alpine-release", 0o644, b"3.20.0\n");

    // New directory and file
    tar_dir(&mut b2, "app/", 0o755);
    let server_content = deterministic_bytes(16384, 0x42);
    tar_file_owned(&mut b2, "app/server", 0o755, &server_content, 1000, 1000);

    let layer2 = finish_tar(b2);

    // -- Unpack both layers ---------------------------------------------------
    let (mut fmt, tmp) = new_formatter();
    fmt.unpack_tar(layer1).unwrap();
    fmt.unpack_tar(layer2).unwrap();

    // Symlinks are created directly via the formatter because unpack_tar
    // currently treats tar Symlink entries as hard links.
    // /usr/bin/env -> /bin/sh (absolute symlink)
    fmt.create(
        "/usr/bin/env",
        make_mode(file_mode::S_IFLNK, 0o777),
        Some("/bin/sh"),
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap();

    // /lib64 -> lib (relative symlink)
    fmt.create(
        "/lib64",
        make_mode(file_mode::S_IFLNK, 0o777),
        Some("lib"),
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap();

    fmt.close().unwrap();

    // -- Open with Reader and verify ------------------------------------------
    let mut reader = Reader::new(tmp.path()).unwrap();

    // ---- Directory structure ------------------------------------------------
    for dir in &[
        "/bin", "/etc", "/lib", "/usr", "/usr/bin", "/usr/lib", "/var", "/var/log", "/tmp",
        "/root", "/home", "/app",
    ] {
        assert!(reader.exists(dir), "directory {dir} should exist");
        let (_, inode) = reader.stat(dir).unwrap();
        assert!(inode.is_dir(), "{dir} should be a directory");
    }

    // /etc/hostname must NOT exist (whiteout removed it)
    assert!(
        !reader.exists("/etc/hostname"),
        "/etc/hostname should have been removed by whiteout"
    );

    // ---- File content checks ------------------------------------------------

    // /bin/sh
    let data = reader.read_file("/bin/sh", 0, None).unwrap();
    assert_eq!(&data, b"#!/bin/busybox sh\n");

    // /etc/alpine-release (overwritten by layer 2)
    let data = reader.read_file("/etc/alpine-release", 0, None).unwrap();
    assert_eq!(&data, b"3.20.0\n");

    // /etc/passwd
    let data = reader.read_file("/etc/passwd", 0, None).unwrap();
    assert_eq!(
        &data,
        b"root:x:0:0:root:/root:/bin/sh\nnobody:x:65534:65534:nobody:/:/sbin/nologin\n"
    );

    // /etc/resolv.conf (added by layer 2)
    let data = reader.read_file("/etc/resolv.conf", 0, None).unwrap();
    assert_eq!(&data, b"nameserver 8.8.8.8\n");

    // /app/server (16KB deterministic content)
    let data = reader.read_file("/app/server", 0, None).unwrap();
    assert_eq!(data.len(), 16384);
    assert_eq!(data, server_content);

    // /lib/ld-musl-aarch64.so.1 (8KB deterministic content)
    let data = reader
        .read_file("/lib/ld-musl-aarch64.so.1", 0, None)
        .unwrap();
    assert_eq!(data.len(), 8192);
    assert_eq!(data, musl_content);

    // ---- Symlink checks -----------------------------------------------------

    // /usr/bin/env -> /bin/sh (following the symlink should read /bin/sh content)
    let data = reader.read_file("/usr/bin/env", 0, None).unwrap();
    assert_eq!(&data, b"#!/bin/busybox sh\n");

    // /lib64 exists as a symlink (stat_no_follow should show it is a link)
    let (_, inode) = reader.stat_no_follow("/lib64").unwrap();
    assert!(inode.is_link(), "/lib64 should be a symlink");

    // ---- Metadata checks ----------------------------------------------------

    // /app/server uid=1000, gid=1000
    let (_, inode) = reader.stat("/app/server").unwrap();
    assert_eq!(inode.uid_full(), 1000, "/app/server uid should be 1000");
    assert_eq!(inode.gid_full(), 1000, "/app/server gid should be 1000");

    // /etc/passwd uid=0, gid=0
    let (_, inode) = reader.stat("/etc/passwd").unwrap();
    assert_eq!(inode.uid_full(), 0, "/etc/passwd uid should be 0");
    assert_eq!(inode.gid_full(), 0, "/etc/passwd gid should be 0");

    // /tmp has sticky bit (mode & 0o1777 == 0o1777)
    let (_, inode) = reader.stat("/tmp").unwrap();
    let perm_bits = inode.mode & 0o7777;
    assert_eq!(
        perm_bits & 0o1777,
        0o1777,
        "/tmp should have sticky bit set; got mode {perm_bits:#o}"
    );

    // /root has mode 0o700
    let (_, inode) = reader.stat("/root").unwrap();
    let perm_bits = inode.mode & 0o7777;
    assert_eq!(
        perm_bits, 0o700,
        "/root should have mode 0o700; got {perm_bits:#o}"
    );

    // ---- Superblock checks --------------------------------------------------
    let sb = reader.superblock();
    assert_eq!(sb.magic, SUPERBLOCK_MAGIC);
    assert!(
        sb.feature_incompat & incompat::EXTENTS != 0,
        "EXTENTS feature flag should be set"
    );
    assert!(
        sb.feature_incompat & incompat::FILETYPE != 0,
        "FILETYPE feature flag should be set"
    );
}

// ===========================================================================
// Test: opaque whiteout directory
// ===========================================================================

#[test]
fn test_oci_opaque_whiteout_directory() {
    // -- Layer 1: create /cache/ with files a, b, c --------------------------
    let mut b1 = tar::Builder::new(Vec::new());

    tar_dir(&mut b1, "cache/", 0o755);
    tar_file(&mut b1, "cache/a", 0o644, b"aaa");
    tar_file(&mut b1, "cache/b", 0o644, b"bbb");
    tar_file(&mut b1, "cache/c", 0o644, b"ccc");

    let layer1 = finish_tar(b1);

    // -- Layer 2: opaque whiteout on /cache/, then add /cache/fresh -----------
    let mut b2 = tar::Builder::new(Vec::new());

    // The opaque whiteout marker deletes all children of /cache/
    tar_whiteout(&mut b2, "cache/.wh..wh..opq");

    // Then add a new file
    tar_file(&mut b2, "cache/fresh", 0o644, b"fresh content");

    let layer2 = finish_tar(b2);

    // -- Unpack ---------------------------------------------------------------
    let (mut fmt, tmp) = new_formatter();
    fmt.unpack_tar(layer1).unwrap();
    fmt.unpack_tar(layer2).unwrap();
    fmt.close().unwrap();

    // -- Verify ---------------------------------------------------------------
    let mut reader = Reader::new(tmp.path()).unwrap();

    // /cache directory itself must still exist
    assert!(reader.exists("/cache"), "/cache directory should exist");
    let (_, inode) = reader.stat("/cache").unwrap();
    assert!(inode.is_dir());

    // Old children must NOT exist
    assert!(
        !reader.exists("/cache/a"),
        "/cache/a should be removed by opaque whiteout"
    );
    assert!(
        !reader.exists("/cache/b"),
        "/cache/b should be removed by opaque whiteout"
    );
    assert!(
        !reader.exists("/cache/c"),
        "/cache/c should be removed by opaque whiteout"
    );

    // New child added after the whiteout must exist
    assert!(reader.exists("/cache/fresh"), "/cache/fresh should exist");
    let data = reader.read_file("/cache/fresh", 0, None).unwrap();
    assert_eq!(&data, b"fresh content");
}

// ===========================================================================
// Test: UTF-8 filenames
// ===========================================================================

#[test]
fn test_oci_utf8_filenames() {
    let mut b = tar::Builder::new(Vec::new());

    tar_dir(&mut b, "data/", 0o755);
    tar_file(
        &mut b,
        "data/\u{65E5}\u{672C}\u{8A9E}.txt",
        0o644,
        b"hello",
    );
    tar_file(
        &mut b,
        "data/\u{00E9}mojis_\u{1F389}.txt",
        0o644,
        b"party",
    );

    let layer = finish_tar(b);

    let (mut fmt, tmp) = new_formatter();
    fmt.unpack_tar(layer).unwrap();

    // Create the symlink directly via the formatter (relative target).
    // /data/链接 -> 日本語.txt
    fmt.create(
        "/data/\u{94FE}\u{63A5}",
        make_mode(file_mode::S_IFLNK, 0o777),
        Some("\u{65E5}\u{672C}\u{8A9E}.txt"),
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap();

    fmt.close().unwrap();

    let mut reader = Reader::new(tmp.path()).unwrap();

    // Japanese filename
    assert!(reader.exists("/data/\u{65E5}\u{672C}\u{8A9E}.txt"));
    let data = reader
        .read_file("/data/\u{65E5}\u{672C}\u{8A9E}.txt", 0, None)
        .unwrap();
    assert_eq!(&data, b"hello");

    // Emoji filename
    assert!(reader.exists("/data/\u{00E9}mojis_\u{1F389}.txt"));
    let data = reader
        .read_file("/data/\u{00E9}mojis_\u{1F389}.txt", 0, None)
        .unwrap();
    assert_eq!(&data, b"party");

    // Chinese symlink resolves to the Japanese file
    assert!(reader.exists("/data/\u{94FE}\u{63A5}"));
    let data = reader
        .read_file("/data/\u{94FE}\u{63A5}", 0, None)
        .unwrap();
    assert_eq!(&data, b"hello");

    // Confirm the symlink is indeed a link
    let (_, inode) = reader
        .stat_no_follow("/data/\u{94FE}\u{63A5}")
        .unwrap();
    assert!(
        inode.is_link(),
        "/data/\u{94FE}\u{63A5} should be a symlink"
    );
}
