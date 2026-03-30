// Minimal reproduction tests for 4 bugs reported by Codex.
// Each test is designed to trigger the specific bug if it exists.

use ext4::constants::*;
use ext4::{Formatter, Reader};
use tempfile::NamedTempFile;

fn new_formatter() -> (Formatter, NamedTempFile) {
    let tmp = NamedTempFile::new().unwrap();
    let fmt = Formatter::new(tmp.path(), 4096, 256 * 1024).unwrap();
    (fmt, tmp)
}

// ═══════════════════════════════════════════════════════════════════════
// Bug 1: unlink original path destroys inode still referenced by hardlink
//
// create("/original") -> link("/alias", "/original") -> unlink("/original")
// Expected: /alias should still be readable with the original content.
// Bug claim: inode gets zeroed, /alias disappears.
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn bug1_unlink_original_preserves_hardlink() {
    let (mut fmt, tmp) = new_formatter();

    fmt.create(
        "/original",
        make_mode(file_mode::S_IFREG, 0o644),
        None, None,
        Some(&mut "hardlink content".as_bytes()),
        None, None, None,
    ).unwrap();

    fmt.link("/alias", "/original").unwrap();
    fmt.unlink("/original", false).unwrap();
    fmt.close().unwrap();

    let mut reader = Reader::new(tmp.path()).unwrap();

    // /alias should still exist and be readable.
    assert!(reader.exists("/alias"), "/alias should exist after unlinking /original");
    let data = reader.read_file("/alias", 0, None).unwrap();
    assert_eq!(&data, b"hardlink content", "/alias content should be intact");

    // /original should be gone.
    assert!(!reader.exists("/original"), "/original should be gone");
}

// ═══════════════════════════════════════════════════════════════════════
// Bug 2: unpack_tar() treats symlinks as hardlinks
//
// tar crate: symlink entries have entry_type == Symlink AND link_name()
// returns Some(...). The code at unpack.rs:61 checks link_name() first,
// which fires for symlinks too, causing them to be deferred as hardlinks.
//
// Expected: symlink in tar -> S_IFLNK inode in ext4
// Bug claim: symlink becomes a regular file (hardlink logic)
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn bug2_unpack_tar_symlink_not_misclassified() {
    use std::io::Cursor;

    let mut tar_buf = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_buf);

        // Regular file.
        let content = b"target content";
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_mode(0o644);
        header.set_size(content.len() as u64);
        header.set_cksum();
        builder.append_data(&mut header, "target.txt", &content[..]).unwrap();

        // Symlink entry.
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_mode(0o777);
        header.set_size(0);
        header.set_cksum();
        builder.append_link(&mut header, "my_symlink", "/target.txt").unwrap();

        builder.finish().unwrap();
    }

    let (mut fmt, tmp) = new_formatter();
    fmt.unpack_tar(Cursor::new(&tar_buf)).unwrap();
    fmt.close().unwrap();

    let mut reader = Reader::new(tmp.path()).unwrap();

    assert!(reader.exists("/target.txt"));
    assert!(reader.exists("/my_symlink"));

    // The crucial check: stat_no_follow should show a symlink, not a regular file.
    let (_, inode) = reader.stat_no_follow("/my_symlink").unwrap();
    assert!(
        is_link(inode.mode),
        "my_symlink should be a symbolic link, but mode = {:#06x}",
        inode.mode
    );

    // Reading through the symlink should return the target's content.
    let data = reader.read_file("/my_symlink", 0, None).unwrap();
    assert_eq!(&data, b"target content");
}

// ═══════════════════════════════════════════════════════════════════════
// Bug 3: non-4096 block size produces broken image
//
// The formatter previously accepted arbitrary block sizes but hardcoded
// log_block_size=2 (4096) in the superblock, producing an internally
// contradictory image.  The fix restricts block_size to 4096 only.
//
// This test verifies:
// (a) non-4096 block sizes are rejected at construction time.
// (b) 4096 writes the correct log_block_size.
// ═══════════════════════════════════════════════════════════════════════

#[test]
#[should_panic(expected = "only 4096-byte block size is supported")]
fn bug3_non_4096_block_size_rejected() {
    let tmp = NamedTempFile::new().unwrap();
    let _ = Formatter::new(tmp.path(), 1024, 256 * 1024);
}

#[test]
fn bug3_4096_block_size_writes_correct_log() {
    let (mut fmt, tmp) = new_formatter(); // uses 4096
    fmt.create(
        "/hello.txt",
        make_mode(file_mode::S_IFREG, 0o644),
        None, None,
        Some(&mut "hello".as_bytes()),
        None, None, None,
    ).unwrap();
    fmt.close().unwrap();

    let reader = Reader::new(tmp.path()).unwrap();
    assert_eq!(reader.superblock().log_block_size, 2);
}

// ═══════════════════════════════════════════════════════════════════════
// Bug 4: recursive mkdir-p through existing symlink silently converts it
//
// If /foo is a symlink and we create("/foo/bar/file", S_IFREG, ...),
// the mkdir-p logic tries to ensure /foo exists as a directory. The
// "already exists + is_link" path at line 217 accepts it and overwrites
// the mode to S_IFDIR, destroying the symlink.
//
// Expected: error or proper handling.
// Bug claim: symlink silently turned into directory.
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn bug4_mkdir_p_through_symlink() {
    let (mut fmt, tmp) = new_formatter();

    // Create a real directory and a symlink pointing to it.
    fmt.create(
        "/real_dir",
        make_mode(file_mode::S_IFDIR, 0o755),
        None, None, None, None, None, None,
    ).unwrap();

    fmt.create(
        "/sym",
        make_mode(file_mode::S_IFLNK, 0o777),
        Some("/real_dir"),
        None, None, None, None, None,
    ).unwrap();

    // Now create a file under /sym/... which triggers mkdir-p on /sym.
    // Since /sym is a symlink (not a directory), the formatter cannot create
    // a child under it and must return an error.  It must NOT silently
    // convert /sym from a symlink into a directory.
    let result = fmt.create(
        "/sym/child.txt",
        make_mode(file_mode::S_IFREG, 0o644),
        None, None,
        Some(&mut "data".as_bytes()),
        None, None, None,
    );
    assert!(result.is_err(), "creating under a symlink path should fail");

    fmt.close().unwrap();

    let mut reader = Reader::new(tmp.path()).unwrap();

    // /sym should still be a symlink, not a directory.
    let (_, sym_inode) = reader.stat_no_follow("/sym").unwrap();
    assert!(
        is_link(sym_inode.mode),
        "/sym should remain a symlink after mkdir-p, but mode = {:#06x} (is_dir={})",
        sym_inode.mode,
        is_dir(sym_inode.mode),
    );
}
