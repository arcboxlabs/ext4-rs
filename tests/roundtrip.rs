// Integration tests: create ext4 images with the Formatter, then verify
// them with the Reader.

use ext4::constants::*;
use ext4::{Formatter, Reader};
use tempfile::NamedTempFile;

/// Helper: create a formatter backed by a temporary file.
fn new_formatter() -> (Formatter, NamedTempFile) {
    let tmp = NamedTempFile::new().unwrap();
    let fmt = Formatter::new(tmp.path(), 4096, 256 * 1024).unwrap();
    (fmt, tmp)
}

#[test]
fn test_empty_filesystem() {
    let (fmt, tmp) = new_formatter();
    fmt.close().unwrap();

    let mut reader = Reader::new(tmp.path()).unwrap();
    let sb = reader.superblock();

    assert_eq!(sb.magic, SUPERBLOCK_MAGIC);
    assert_eq!(sb.log_block_size, 2); // 4096 bytes
    assert_eq!(sb.first_ino, FIRST_INODE);

    // Root directory must exist.
    assert!(reader.exists("/"));

    // /lost+found must exist (required by e2fsck).
    assert!(reader.exists("/lost+found"));
}

#[test]
fn test_create_and_read_file() {
    let (mut fmt, tmp) = new_formatter();

    let content = b"Hello, ext4 from Rust!";
    fmt.create(
        "/greeting.txt",
        make_mode(file_mode::S_IFREG, 0o644),
        None,
        None,
        Some(&mut &content[..]),
        Some(1000),
        Some(1000),
        None,
    )
    .unwrap();

    fmt.close().unwrap();

    let mut reader = Reader::new(tmp.path()).unwrap();

    // Verify the file exists.
    assert!(reader.exists("/greeting.txt"));

    // Read the content back.
    let data = reader.read_file("/greeting.txt", 0, None).unwrap();
    assert_eq!(&data, content);

    // Read with offset.
    let partial = reader.read_file("/greeting.txt", 7, Some(4)).unwrap();
    assert_eq!(&partial, b"ext4");

    // Stat the file.
    let (_, inode) = reader.stat("/greeting.txt").unwrap();
    assert!(is_reg(inode.mode));
    assert_eq!(inode.uid_full(), 1000);
    assert_eq!(inode.gid_full(), 1000);
}

#[test]
fn test_nested_directories() {
    let (mut fmt, tmp) = new_formatter();

    // create() should auto-create parents.
    fmt.create(
        "/a/b/c/d",
        make_mode(file_mode::S_IFDIR, 0o755),
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap();

    fmt.create(
        "/a/b/c/d/file.txt",
        make_mode(file_mode::S_IFREG, 0o644),
        None,
        None,
        Some(&mut "deep".as_bytes()),
        None,
        None,
        None,
    )
    .unwrap();

    fmt.close().unwrap();

    let mut reader = Reader::new(tmp.path()).unwrap();

    assert!(reader.exists("/a"));
    assert!(reader.exists("/a/b"));
    assert!(reader.exists("/a/b/c"));
    assert!(reader.exists("/a/b/c/d"));
    assert!(reader.exists("/a/b/c/d/file.txt"));

    let data = reader.read_file("/a/b/c/d/file.txt", 0, None).unwrap();
    assert_eq!(&data, b"deep");
}

#[test]
fn test_symlinks() {
    let (mut fmt, tmp) = new_formatter();

    // Create a file.
    fmt.create(
        "/target.txt",
        make_mode(file_mode::S_IFREG, 0o644),
        None,
        None,
        Some(&mut "target content".as_bytes()),
        None,
        None,
        None,
    )
    .unwrap();

    // Create a short symlink (inline, < 60 bytes).
    fmt.create(
        "/short_link",
        make_mode(file_mode::S_IFLNK, 0o777),
        Some("/target.txt"),
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap();

    // Create a long symlink (> 60 bytes, stored in data blocks).
    let long_target = "/a/very/deeply/nested/path/that/is/longer/than/sixty/bytes/target.txt";
    fmt.create(
        "/a/very/deeply/nested/path/that/is/longer/than/sixty/bytes",
        make_mode(file_mode::S_IFDIR, 0o755),
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap();
    fmt.create(
        long_target,
        make_mode(file_mode::S_IFREG, 0o644),
        None,
        None,
        Some(&mut "long target".as_bytes()),
        None,
        None,
        None,
    )
    .unwrap();
    fmt.create(
        "/long_link",
        make_mode(file_mode::S_IFLNK, 0o777),
        Some(long_target),
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap();

    fmt.close().unwrap();

    let mut reader = Reader::new(tmp.path()).unwrap();

    // Short symlink should resolve to the target content.
    let data = reader.read_file("/short_link", 0, None).unwrap();
    assert_eq!(&data, b"target content");

    // Long symlink should also resolve.
    let data = reader.read_file("/long_link", 0, None).unwrap();
    assert_eq!(&data, b"long target");

    // stat without following symlinks should show a link.
    let (_, inode) = reader.stat_no_follow("/short_link").unwrap();
    assert!(is_link(inode.mode));
}

#[test]
fn test_list_directory() {
    let (mut fmt, tmp) = new_formatter();

    fmt.create("/dir", make_mode(file_mode::S_IFDIR, 0o755), None, None, None, None, None, None)
        .unwrap();
    fmt.create(
        "/dir/alpha",
        make_mode(file_mode::S_IFREG, 0o644),
        None,
        None,
        Some(&mut "a".as_bytes()),
        None,
        None,
        None,
    )
    .unwrap();
    fmt.create(
        "/dir/beta",
        make_mode(file_mode::S_IFREG, 0o644),
        None,
        None,
        Some(&mut "b".as_bytes()),
        None,
        None,
        None,
    )
    .unwrap();
    fmt.create(
        "/dir/gamma",
        make_mode(file_mode::S_IFREG, 0o644),
        None,
        None,
        Some(&mut "g".as_bytes()),
        None,
        None,
        None,
    )
    .unwrap();

    fmt.close().unwrap();

    let mut reader = Reader::new(tmp.path()).unwrap();
    let entries = reader.list_dir("/dir").unwrap();

    // Should contain exactly our 3 files, sorted.
    assert_eq!(entries, vec!["alpha", "beta", "gamma"]);
}

#[test]
fn test_hard_links_roundtrip() {
    let (mut fmt, tmp) = new_formatter();

    fmt.create(
        "/original.txt",
        make_mode(file_mode::S_IFREG, 0o644),
        None,
        None,
        Some(&mut "shared content".as_bytes()),
        None,
        None,
        None,
    )
    .unwrap();

    fmt.link("/linked.txt", "/original.txt").unwrap();
    fmt.close().unwrap();

    let mut reader = Reader::new(tmp.path()).unwrap();

    // Both paths should return the same content.
    let data1 = reader.read_file("/original.txt", 0, None).unwrap();
    let data2 = reader.read_file("/linked.txt", 0, None).unwrap();
    assert_eq!(data1, data2);
    assert_eq!(&data1, b"shared content");
}

#[test]
fn test_unpack_tar() {
    use std::io::Cursor;

    // Build a tar archive in memory.
    let mut tar_buf = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_buf);

        // Add a directory.
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Directory);
        header.set_mode(0o755);
        header.set_size(0);
        header.set_cksum();
        builder.append_data(&mut header, "etc/", &[] as &[u8]).unwrap();

        // Add a file.
        let content = b"root:x:0:0:root:/root:/bin/bash\n";
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_mode(0o644);
        header.set_size(content.len() as u64);
        header.set_uid(0);
        header.set_gid(0);
        header.set_cksum();
        builder.append_data(&mut header, "etc/passwd", &content[..]).unwrap();

        // Add a symlink.
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_mode(0o777);
        header.set_size(0);
        header.set_cksum();
        builder
            .append_link(&mut header, "etc/passwd-link", "/etc/passwd")
            .unwrap();

        builder.finish().unwrap();
    }

    let (mut fmt, tmp) = new_formatter();
    fmt.unpack_tar(Cursor::new(&tar_buf)).unwrap();
    fmt.close().unwrap();

    let mut reader = Reader::new(tmp.path()).unwrap();

    assert!(reader.exists("/etc"));
    assert!(reader.exists("/etc/passwd"));

    let data = reader.read_file("/etc/passwd", 0, None).unwrap();
    assert_eq!(&data, b"root:x:0:0:root:/root:/bin/bash\n");

    // Symlink should resolve.
    let data = reader.read_file("/etc/passwd-link", 0, None).unwrap();
    assert_eq!(&data, b"root:x:0:0:root:/root:/bin/bash\n");
}

#[test]
fn test_oci_whiteout() {
    use std::io::Cursor;

    // Layer 1: create /etc/shadow.
    let mut layer1_buf = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut layer1_buf);

        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Directory);
        header.set_mode(0o755);
        header.set_size(0);
        header.set_cksum();
        builder.append_data(&mut header, "etc/", &[] as &[u8]).unwrap();

        let content = b"secret";
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_mode(0o600);
        header.set_size(content.len() as u64);
        header.set_cksum();
        builder.append_data(&mut header, "etc/shadow", &content[..]).unwrap();

        builder.finish().unwrap();
    }

    // Layer 2: whiteout /etc/shadow.
    let mut layer2_buf = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut layer2_buf);

        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_mode(0o644);
        header.set_size(0);
        header.set_cksum();
        builder
            .append_data(&mut header, "etc/.wh.shadow", &[] as &[u8])
            .unwrap();

        builder.finish().unwrap();
    }

    let (mut fmt, tmp) = new_formatter();
    fmt.unpack_tar(Cursor::new(&layer1_buf)).unwrap();

    // Before layer 2, /etc/shadow should exist.
    // (We can't use Reader mid-format, so we just apply layer 2.)

    fmt.unpack_tar(Cursor::new(&layer2_buf)).unwrap();
    fmt.close().unwrap();

    let mut reader = Reader::new(tmp.path()).unwrap();

    // /etc should still exist.
    assert!(reader.exists("/etc"));

    // /etc/shadow should have been deleted by the whiteout.
    assert!(!reader.exists("/etc/shadow"));
}
