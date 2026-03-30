// Integration tests: create ext4 images with the Formatter, then verify
// them with the Reader.

use arcbox_ext4::constants::*;
use arcbox_ext4::{Formatter, Reader};
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

// ---------------------------------------------------------------------------
// Empty file
// ---------------------------------------------------------------------------

#[test]
fn test_empty_file() {
    let (mut fmt, tmp) = new_formatter();

    // Create a regular file with no data (None for the reader argument).
    fmt.create(
        "/empty.bin",
        make_mode(file_mode::S_IFREG, 0o644),
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap();

    fmt.close().unwrap();

    let mut reader = Reader::new(tmp.path()).unwrap();

    // The file should exist.
    assert!(reader.exists("/empty.bin"));

    // Reading should return an empty vector.
    let data = reader.read_file("/empty.bin", 0, None).unwrap();
    assert!(data.is_empty());

    // Stat should show size 0.
    let (_, inode) = reader.stat("/empty.bin").unwrap();
    assert!(is_reg(inode.mode));
    assert_eq!(inode.file_size(), 0);
}

// ---------------------------------------------------------------------------
// Zero-byte read / out-of-bounds read
// ---------------------------------------------------------------------------

#[test]
fn test_zero_byte_read() {
    let (mut fmt, tmp) = new_formatter();

    let content = b"some data here";
    fmt.create(
        "/data.bin",
        make_mode(file_mode::S_IFREG, 0o644),
        None,
        None,
        Some(&mut &content[..]),
        None,
        None,
        None,
    )
    .unwrap();

    fmt.close().unwrap();

    let mut reader = Reader::new(tmp.path()).unwrap();

    // read_file with count=Some(0) should return empty.
    let data = reader.read_file("/data.bin", 0, Some(0)).unwrap();
    assert!(data.is_empty());

    // read_file with offset past EOF should return empty.
    let data = reader.read_file("/data.bin", 9999, None).unwrap();
    assert!(data.is_empty());

    // read_file with offset past EOF and explicit count should return empty.
    let data = reader.read_file("/data.bin", 9999, Some(10)).unwrap();
    assert!(data.is_empty());

    // read_file_into with a zero-length buffer should return 0.
    let mut buf = [];
    let n = reader.read_file_into("/data.bin", &mut buf, 0).unwrap();
    assert_eq!(n, 0);
}

// ---------------------------------------------------------------------------
// Partial reads at various offsets
// ---------------------------------------------------------------------------

#[test]
fn test_partial_read() {
    let (mut fmt, tmp) = new_formatter();

    let content = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    fmt.create(
        "/alpha.txt",
        make_mode(file_mode::S_IFREG, 0o644),
        None,
        None,
        Some(&mut &content[..]),
        None,
        None,
        None,
    )
    .unwrap();

    fmt.close().unwrap();

    let mut reader = Reader::new(tmp.path()).unwrap();

    // Read the first 5 bytes.
    let data = reader.read_file("/alpha.txt", 0, Some(5)).unwrap();
    assert_eq!(&data, b"ABCDE");

    // Read 3 bytes starting at offset 10.
    let data = reader.read_file("/alpha.txt", 10, Some(3)).unwrap();
    assert_eq!(&data, b"KLM");

    // Read from the last 3 bytes.
    let data = reader.read_file("/alpha.txt", 23, Some(10)).unwrap();
    assert_eq!(&data, b"XYZ");

    // Read the entire file with offset 0 and count = file length.
    let data = reader
        .read_file("/alpha.txt", 0, Some(content.len()))
        .unwrap();
    assert_eq!(&data, content);

    // Read 1 byte at each position.
    for (i, &expected) in content.iter().enumerate() {
        let data = reader.read_file("/alpha.txt", i as u64, Some(1)).unwrap();
        assert_eq!(data.len(), 1);
        assert_eq!(data[0], expected, "mismatch at offset {}", i);
    }

    // read_file_into: read the middle portion.
    let mut buf = [0u8; 4];
    let n = reader.read_file_into("/alpha.txt", &mut buf, 5).unwrap();
    assert_eq!(n, 4);
    assert_eq!(&buf, b"FGHI");
}

// ---------------------------------------------------------------------------
// Large file spanning multiple blocks
// ---------------------------------------------------------------------------

#[test]
fn test_large_file_multi_block() {
    let (mut fmt, tmp) = new_formatter();

    // Create a deterministic pattern larger than one 4096-byte block.
    // 20 KiB = 5 blocks.
    let size = 20 * 1024usize;
    let pattern: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();

    fmt.create(
        "/big.bin",
        make_mode(file_mode::S_IFREG, 0o644),
        None,
        None,
        Some(&mut pattern.as_slice()),
        None,
        None,
        None,
    )
    .unwrap();

    fmt.close().unwrap();

    let mut reader = Reader::new(tmp.path()).unwrap();

    // Read the full file and verify byte-by-byte.
    let data = reader.read_file("/big.bin", 0, None).unwrap();
    assert_eq!(data.len(), size);
    assert_eq!(data, pattern);

    // Stat should show the correct size.
    let (_, inode) = reader.stat("/big.bin").unwrap();
    assert_eq!(inode.file_size(), size as u64);

    // Read a sub-range that spans a block boundary (offset 4090, 20 bytes).
    let data = reader.read_file("/big.bin", 4090, Some(20)).unwrap();
    assert_eq!(data, &pattern[4090..4110]);

    // Read the last 100 bytes.
    let data = reader
        .read_file("/big.bin", (size - 100) as u64, None)
        .unwrap();
    assert_eq!(data, &pattern[size - 100..]);

    // read_file_into spanning the full file.
    let mut buf = vec![0u8; size];
    let n = reader.read_file_into("/big.bin", &mut buf, 0).unwrap();
    assert_eq!(n, size);
    assert_eq!(buf, pattern);
}

// =========================================================================
// Error path: read non-existent path
// =========================================================================

#[test]
fn test_read_nonexistent_path() {
    let (mut fmt, tmp) = new_formatter();

    // Create a file so the filesystem is not completely empty.
    fmt.create(
        "/existing.txt",
        make_mode(file_mode::S_IFREG, 0o644),
        None,
        None,
        Some(&mut "data".as_bytes()),
        None,
        None,
        None,
    )
    .unwrap();

    fmt.close().unwrap();

    let mut reader = Reader::new(tmp.path()).unwrap();

    // exists() should return false for nonexistent paths.
    assert!(!reader.exists("/nonexistent"));
    assert!(!reader.exists("/no/such/path"));

    // stat() should return PathNotFound.
    let err = reader.stat("/nonexistent").unwrap_err();
    assert!(
        matches!(err, arcbox_ext4::error::ReadError::PathNotFound(_)),
        "expected PathNotFound, got: {err:?}"
    );

    // read_file() should return PathNotFound.
    let err = reader.read_file("/nonexistent", 0, None).unwrap_err();
    assert!(
        matches!(err, arcbox_ext4::error::ReadError::PathNotFound(_)),
        "expected PathNotFound, got: {err:?}"
    );

    // list_dir() should return PathNotFound.
    let err = reader.list_dir("/nonexistent").unwrap_err();
    assert!(
        matches!(err, arcbox_ext4::error::ReadError::PathNotFound(_)),
        "expected PathNotFound, got: {err:?}"
    );
}

// =========================================================================
// Error path: read_file on a directory
// =========================================================================

#[test]
fn test_read_file_on_directory() {
    let (mut fmt, tmp) = new_formatter();

    fmt.create(
        "/mydir",
        make_mode(file_mode::S_IFDIR, 0o755),
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap();

    fmt.close().unwrap();

    let mut reader = Reader::new(tmp.path()).unwrap();

    // read_file on a directory should return IsDirectory.
    let err = reader.read_file("/mydir", 0, None).unwrap_err();
    assert!(
        matches!(err, arcbox_ext4::error::ReadError::IsDirectory(_)),
        "expected IsDirectory, got: {err:?}"
    );
}

// =========================================================================
// Error path: list_dir on a file
// =========================================================================

#[test]
fn test_list_dir_on_file() {
    let (mut fmt, tmp) = new_formatter();

    fmt.create(
        "/file.txt",
        make_mode(file_mode::S_IFREG, 0o644),
        None,
        None,
        Some(&mut "content".as_bytes()),
        None,
        None,
        None,
    )
    .unwrap();

    fmt.close().unwrap();

    let mut reader = Reader::new(tmp.path()).unwrap();

    // list_dir on a regular file should return NotADirectory.
    let err = reader.list_dir("/file.txt").unwrap_err();
    assert!(
        matches!(err, arcbox_ext4::error::ReadError::NotADirectory(_)),
        "expected NotADirectory, got: {err:?}"
    );
}

// =========================================================================
// Error path: invalid / corrupted image
// =========================================================================

#[test]
fn test_invalid_image() {
    use std::io::Write;

    // All-zeros image.
    let tmp_zeros = NamedTempFile::new().unwrap();
    {
        let mut f = std::fs::File::create(tmp_zeros.path()).unwrap();
        f.write_all(&[0u8; 4096]).unwrap();
    }
    match Reader::new(tmp_zeros.path()) {
        Err(arcbox_ext4::error::ReadError::InvalidSuperBlock) => {} // expected
        Err(other) => panic!("expected InvalidSuperBlock for all-zeros, got: {other:?}"),
        Ok(_) => panic!("expected InvalidSuperBlock for all-zeros, but got Ok"),
    }

    // Random-ish bytes (deterministic pattern, not real ext4).
    let tmp_garbage = NamedTempFile::new().unwrap();
    {
        let mut f = std::fs::File::create(tmp_garbage.path()).unwrap();
        let garbage: Vec<u8> = (0u8..=255).cycle().take(4096).collect();
        f.write_all(&garbage).unwrap();
    }
    match Reader::new(tmp_garbage.path()) {
        Err(arcbox_ext4::error::ReadError::InvalidSuperBlock) => {} // expected
        Err(other) => panic!("expected InvalidSuperBlock for garbage, got: {other:?}"),
        Ok(_) => panic!("expected InvalidSuperBlock for garbage, but got Ok"),
    }
}

// =========================================================================
// Symlink loop detection
// =========================================================================

#[test]
fn test_symlink_loop_detection() {
    let (mut fmt, tmp) = new_formatter();

    // Create /link_a -> /link_b and /link_b -> /link_a (a cycle).
    fmt.create(
        "/link_a",
        make_mode(file_mode::S_IFLNK, 0o777),
        Some("/link_b"),
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap();

    fmt.create(
        "/link_b",
        make_mode(file_mode::S_IFLNK, 0o777),
        Some("/link_a"),
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap();

    fmt.close().unwrap();

    let mut reader = Reader::new(tmp.path()).unwrap();

    // Following the symlink cycle should produce SymlinkLoop.
    let err = reader.read_file("/link_a", 0, None).unwrap_err();
    assert!(
        matches!(err, arcbox_ext4::error::ReadError::SymlinkLoop(_)),
        "expected SymlinkLoop, got: {err:?}"
    );

    let err = reader.stat("/link_b").unwrap_err();
    assert!(
        matches!(err, arcbox_ext4::error::ReadError::SymlinkLoop(_)),
        "expected SymlinkLoop, got: {err:?}"
    );
}

// =========================================================================
// Symlink chain (not a loop)
// =========================================================================

#[test]
fn test_symlink_chain() {
    let (mut fmt, tmp) = new_formatter();

    // Create: /a -> /b, /b -> /c, /c -> /real_file.
    fmt.create(
        "/real_file",
        make_mode(file_mode::S_IFREG, 0o644),
        None,
        None,
        Some(&mut "chained content".as_bytes()),
        None,
        None,
        None,
    )
    .unwrap();

    fmt.create(
        "/c",
        make_mode(file_mode::S_IFLNK, 0o777),
        Some("/real_file"),
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap();

    fmt.create(
        "/b",
        make_mode(file_mode::S_IFLNK, 0o777),
        Some("/c"),
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap();

    fmt.create(
        "/a",
        make_mode(file_mode::S_IFLNK, 0o777),
        Some("/b"),
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap();

    fmt.close().unwrap();

    let mut reader = Reader::new(tmp.path()).unwrap();

    // Reading through the chain should resolve to the real file.
    let data = reader.read_file("/a", 0, None).unwrap();
    assert_eq!(&data, b"chained content");

    let data = reader.read_file("/b", 0, None).unwrap();
    assert_eq!(&data, b"chained content");

    let data = reader.read_file("/c", 0, None).unwrap();
    assert_eq!(&data, b"chained content");

    // stat on /a should give us the inode of /real_file (following symlinks).
    let (_, inode_a) = reader.stat("/a").unwrap();
    let (_, inode_real) = reader.stat("/real_file").unwrap();
    assert!(is_reg(inode_a.mode));
    assert_eq!(inode_a.file_size(), inode_real.file_size());
}

// =========================================================================
// Relative symlinks
// =========================================================================

#[test]
fn test_relative_symlinks() {
    let (mut fmt, tmp) = new_formatter();

    // Create /dir/file.txt with content.
    fmt.create(
        "/dir/file.txt",
        make_mode(file_mode::S_IFREG, 0o644),
        None,
        None,
        Some(&mut "relative target".as_bytes()),
        None,
        None,
        None,
    )
    .unwrap();

    // Create /dir/rel_link -> file.txt  (relative symlink within same dir).
    fmt.create(
        "/dir/rel_link",
        make_mode(file_mode::S_IFLNK, 0o777),
        Some("file.txt"),
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap();

    // Create /other_link -> dir/file.txt  (relative symlink from root).
    fmt.create(
        "/other_link",
        make_mode(file_mode::S_IFLNK, 0o777),
        Some("dir/file.txt"),
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap();

    fmt.close().unwrap();

    let mut reader = Reader::new(tmp.path()).unwrap();

    // Direct read.
    let data = reader.read_file("/dir/file.txt", 0, None).unwrap();
    assert_eq!(&data, b"relative target");

    // Relative symlink within the same directory.
    let data = reader.read_file("/dir/rel_link", 0, None).unwrap();
    assert_eq!(&data, b"relative target");

    // Relative symlink from root.
    let data = reader.read_file("/other_link", 0, None).unwrap();
    assert_eq!(&data, b"relative target");

    // stat through relative symlinks should show a regular file.
    let (_, inode) = reader.stat("/dir/rel_link").unwrap();
    assert!(is_reg(inode.mode));

    let (_, inode) = reader.stat("/other_link").unwrap();
    assert!(is_reg(inode.mode));
}

// ===========================================================================
// Xattr end-to-end test
// ===========================================================================

#[test]
fn test_xattr_end_to_end() {
    use std::collections::HashMap;

    let (mut fmt, tmp) = new_formatter();

    let mut xattrs = HashMap::new();
    xattrs.insert("user.mime_type".to_string(), b"text/plain".to_vec());
    xattrs.insert(
        "security.selinux".to_string(),
        b"unconfined_u:object_r:user_home_t:s0\0".to_vec(),
    );

    let content = b"xattr test file";
    fmt.create(
        "/xattr_file.txt",
        make_mode(file_mode::S_IFREG, 0o644),
        None,
        None,
        Some(&mut &content[..]),
        Some(1000),
        Some(1000),
        Some(&xattrs),
    )
    .unwrap();

    fmt.close().unwrap();

    let mut reader = Reader::new(tmp.path()).unwrap();

    // The file itself must be readable.
    let data = reader.read_file("/xattr_file.txt", 0, None).unwrap();
    assert_eq!(&data, content);

    // The Reader does not (yet) expose a high-level xattr API, so we verify
    // at the inode level that the inline_xattrs area has been populated.
    let (_, inode) = reader.stat("/xattr_file.txt").unwrap();

    // The first 4 bytes of inline_xattrs should be the xattr header magic
    // (0xEA020000) if any xattrs were written inline.
    let magic = u32::from_le_bytes([
        inode.inline_xattrs[0],
        inode.inline_xattrs[1],
        inode.inline_xattrs[2],
        inode.inline_xattrs[3],
    ]);
    assert_eq!(magic, XATTR_HEADER_MAGIC, "inline xattr magic mismatch");

    // The rest of the inline_xattrs area must not be all zeros (entries exist).
    assert!(
        inode.inline_xattrs[4..].iter().any(|&b| b != 0),
        "inline xattr data should be non-zero when xattrs are set"
    );

    // Also verify that uid/gid survived the roundtrip.
    assert_eq!(inode.uid_full(), 1000);
    assert_eq!(inode.gid_full(), 1000);
}

// ===========================================================================
// Multi block group test
// ===========================================================================

#[test]
fn test_multi_block_group() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    // 256 MiB -- spans 2 block groups (each block group = 32768 * 4096 = 128 MiB).
    let min_disk_size: u64 = 256 * 1024 * 1024;
    let mut fmt = Formatter::new(tmp.path(), 4096, min_disk_size).unwrap();

    // Write a handful of files -- the sparse file handles the bulk of the space.
    for i in 0..10 {
        let name = format!("/file_{i}.txt");
        let content = format!("content of file {i}");
        fmt.create(
            &name,
            make_mode(file_mode::S_IFREG, 0o644),
            None,
            None,
            Some(&mut content.as_bytes()),
            None,
            None,
            None,
        )
        .unwrap();
    }

    fmt.close().unwrap();

    let mut reader = Reader::new(tmp.path()).unwrap();

    // Copy superblock values before any mutable borrows.
    let blocks_count = reader.superblock().blocks_count_lo;
    let magic = reader.superblock().magic;

    // The block count should cover at least 256 MiB / 4096 = 65536 blocks.
    assert!(
        blocks_count >= 65536,
        "expected >= 65536 blocks, got {}",
        blocks_count
    );

    // Verify every file is readable.
    for i in 0..10 {
        let name = format!("/file_{i}.txt");
        let expected = format!("content of file {i}");
        let data = reader.read_file(&name, 0, None).unwrap();
        assert_eq!(data, expected.as_bytes(), "mismatch for {name}");
    }

    // Superblock magic must be intact.
    assert_eq!(magic, SUPERBLOCK_MAGIC);
}

// ===========================================================================
// Large directory test (500 files)
// ===========================================================================

#[test]
fn test_large_directory() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    // 4 MiB is enough for 500 tiny files.
    let mut fmt = Formatter::new(tmp.path(), 4096, 4 * 1024 * 1024).unwrap();

    fmt.create(
        "/bigdir",
        make_mode(file_mode::S_IFDIR, 0o755),
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap();

    let file_count = 500;
    for i in 0..file_count {
        let name = format!("/bigdir/entry_{i:04}.txt");
        let content = format!("data-{i}");
        fmt.create(
            &name,
            make_mode(file_mode::S_IFREG, 0o644),
            None,
            None,
            Some(&mut content.as_bytes()),
            None,
            None,
            None,
        )
        .unwrap();
    }

    fmt.close().unwrap();

    let mut reader = Reader::new(tmp.path()).unwrap();

    // list_dir must return all 500 entries.
    let entries = reader.list_dir("/bigdir").unwrap();
    assert_eq!(
        entries.len(),
        file_count,
        "expected {file_count} entries, got {}",
        entries.len()
    );

    // Spot-check first, middle, and last files.
    for i in [0, 249, 499] {
        let name = format!("/bigdir/entry_{i:04}.txt");
        let expected = format!("data-{i}");
        let data = reader.read_file(&name, 0, None).unwrap();
        assert_eq!(data, expected.as_bytes(), "content mismatch for {name}");
    }
}

// ===========================================================================
// Opaque whiteout test
// ===========================================================================

#[test]
fn test_opaque_whiteout() {
    use std::io::Cursor;

    // Layer 1: create a directory with several files.
    let mut layer1_buf = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut layer1_buf);

        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Directory);
        header.set_mode(0o755);
        header.set_size(0);
        header.set_cksum();
        builder
            .append_data(&mut header, "mydir/", &[] as &[u8])
            .unwrap();

        for name in ["alpha.txt", "beta.txt", "gamma.txt"] {
            let content = format!("content of {name}");
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Regular);
            header.set_mode(0o644);
            header.set_size(content.len() as u64);
            header.set_cksum();
            builder
                .append_data(&mut header, format!("mydir/{name}"), content.as_bytes())
                .unwrap();
        }

        builder.finish().unwrap();
    }

    // Layer 2: opaque whiteout on /mydir -- removes all children.
    let mut layer2_buf = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut layer2_buf);

        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_mode(0o644);
        header.set_size(0);
        header.set_cksum();
        builder
            .append_data(&mut header, "mydir/.wh..wh..opq", &[] as &[u8])
            .unwrap();

        builder.finish().unwrap();
    }

    let (mut fmt, tmp) = new_formatter();
    fmt.unpack_tar(Cursor::new(&layer1_buf)).unwrap();
    fmt.unpack_tar(Cursor::new(&layer2_buf)).unwrap();
    fmt.close().unwrap();

    let mut reader = Reader::new(tmp.path()).unwrap();

    // The directory itself should still exist.
    assert!(reader.exists("/mydir"));

    // All children should have been removed by the opaque whiteout.
    assert!(!reader.exists("/mydir/alpha.txt"));
    assert!(!reader.exists("/mydir/beta.txt"));
    assert!(!reader.exists("/mydir/gamma.txt"));

    // The directory listing should be empty.
    let entries = reader.list_dir("/mydir").unwrap();
    assert!(entries.is_empty(), "expected empty dir, got {entries:?}");
}

// ===========================================================================
// Unpack: hardlink chain in tar
// ===========================================================================

#[test]
fn test_unpack_hardlink_chain() {
    use std::io::Cursor;

    let mut tar_buf = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_buf);

        // Regular file: base.txt
        let content = b"shared via hardlinks";
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_mode(0o644);
        header.set_size(content.len() as u64);
        header.set_cksum();
        builder
            .append_data(&mut header, "base.txt", &content[..])
            .unwrap();

        // Hardlink: link1.txt -> base.txt
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Link);
        header.set_mode(0o644);
        header.set_size(0);
        header.set_cksum();
        builder
            .append_link(&mut header, "link1.txt", "base.txt")
            .unwrap();

        // Hardlink chain: link2.txt -> link1.txt
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Link);
        header.set_mode(0o644);
        header.set_size(0);
        header.set_cksum();
        builder
            .append_link(&mut header, "link2.txt", "link1.txt")
            .unwrap();

        builder.finish().unwrap();
    }

    let (mut fmt, tmp) = new_formatter();
    fmt.unpack_tar(Cursor::new(&tar_buf)).unwrap();
    fmt.close().unwrap();

    let mut reader = Reader::new(tmp.path()).unwrap();

    // All three paths should exist and return the same content.
    let data_base = reader.read_file("/base.txt", 0, None).unwrap();
    let data_link1 = reader.read_file("/link1.txt", 0, None).unwrap();
    let data_link2 = reader.read_file("/link2.txt", 0, None).unwrap();

    assert_eq!(&data_base, b"shared via hardlinks");
    assert_eq!(data_base, data_link1);
    assert_eq!(data_base, data_link2);
}

// ===========================================================================
// Unpack: device files are skipped
// ===========================================================================

#[test]
fn test_unpack_device_files_skipped() {
    use std::io::Cursor;

    let mut tar_buf = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_buf);

        // A regular file to ensure the archive is not completely empty.
        let content = b"normal file";
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_mode(0o644);
        header.set_size(content.len() as u64);
        header.set_cksum();
        builder
            .append_data(&mut header, "normal.txt", &content[..])
            .unwrap();

        // A character device entry (e.g. /dev/null).
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Char);
        header.set_mode(0o666);
        header.set_size(0);
        header.set_cksum();
        builder
            .append_data(&mut header, "dev/null", &[] as &[u8])
            .unwrap();

        // A block device entry.
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Block);
        header.set_mode(0o660);
        header.set_size(0);
        header.set_cksum();
        builder
            .append_data(&mut header, "dev/sda", &[] as &[u8])
            .unwrap();

        builder.finish().unwrap();
    }

    let (mut fmt, tmp) = new_formatter();
    // Unpacking should not fail even with device entries.
    fmt.unpack_tar(Cursor::new(&tar_buf)).unwrap();
    fmt.close().unwrap();

    let mut reader = Reader::new(tmp.path()).unwrap();

    // The regular file should be present.
    assert!(reader.exists("/normal.txt"));
    let data = reader.read_file("/normal.txt", 0, None).unwrap();
    assert_eq!(&data, b"normal file");

    // Device files should have been silently skipped.
    assert!(!reader.exists("/dev/null"));
    assert!(!reader.exists("/dev/sda"));
}

// ===========================================================================
// read_file_into test
// ===========================================================================

#[test]
fn test_read_file_into() {
    let (mut fmt, tmp) = new_formatter();

    let content = b"Hello, read_file_into!";
    fmt.create(
        "/readinto.txt",
        make_mode(file_mode::S_IFREG, 0o644),
        None,
        None,
        Some(&mut &content[..]),
        None,
        None,
        None,
    )
    .unwrap();

    fmt.close().unwrap();

    let mut reader = Reader::new(tmp.path()).unwrap();

    // Exact-size buffer: should read all bytes.
    {
        let mut buf = vec![0u8; content.len()];
        let n = reader.read_file_into("/readinto.txt", &mut buf, 0).unwrap();
        assert_eq!(n, content.len());
        assert_eq!(&buf[..n], content);
    }

    // Small buffer: should return partial data (only buf.len() bytes).
    {
        let mut buf = vec![0u8; 5];
        let n = reader.read_file_into("/readinto.txt", &mut buf, 0).unwrap();
        assert_eq!(n, 5);
        assert_eq!(&buf[..n], &content[..5]);
    }

    // Large buffer: should return only actual file bytes, rest unchanged.
    {
        let mut buf = vec![0xFFu8; 1024];
        let n = reader.read_file_into("/readinto.txt", &mut buf, 0).unwrap();
        assert_eq!(n, content.len());
        assert_eq!(&buf[..n], content);
        // Bytes beyond the file content remain 0xFF.
        assert!(buf[n..].iter().all(|&b| b == 0xFF));
    }

    // Read with offset.
    {
        let mut buf = vec![0u8; 10];
        let n = reader.read_file_into("/readinto.txt", &mut buf, 7).unwrap();
        // "read_file_into!" starts at offset 7.
        assert_eq!(&buf[..n], &content[7..7 + n]);
    }

    // Read with offset past EOF: should return 0 bytes.
    {
        let mut buf = vec![0u8; 10];
        let n = reader
            .read_file_into("/readinto.txt", &mut buf, 9999)
            .unwrap();
        assert_eq!(n, 0);
    }
}
