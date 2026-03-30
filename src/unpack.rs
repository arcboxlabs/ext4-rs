// OCI container image layer unpacking.
//
// Streams a tar archive into the ext4 formatter, handling OCI-specific whiteout
// files (`.wh.*` and `.wh..wh..opq`) and hard-link cycle detection.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::constants::*;
use crate::error::{FormatError, FormatResult};
use crate::formatter::{FileTimestamps, Formatter};
use crate::types::timestamp_now;

impl Formatter {
    /// Unpack a tar archive onto this ext4 filesystem.
    ///
    /// Handles:
    /// - Regular files, directories, and symbolic links
    /// - OCI whiteout files (`.wh.<name>` deletes `<name>`, `.wh..wh..opq`
    ///   deletes all children of the containing directory)
    /// - Hard links with cycle detection
    /// - Preservation of uid/gid, permissions, and timestamps
    pub fn unpack_tar<R: Read>(&mut self, reader: R) -> FormatResult<()> {
        let mut archive = tar::Archive::new(reader);
        let mut hardlinks: HashMap<PathBuf, PathBuf> = HashMap::new();

        for entry_result in archive.entries().map_err(io_to_format)? {
            let mut entry = entry_result.map_err(io_to_format)?;
            let raw_path = entry.path().map_err(io_to_format)?.into_owned();

            let path_str = preprocess_path(&raw_path);
            let path = Path::new(&path_str);

            let basename = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("");

            // ── OCI whiteouts ──
            if basename.starts_with(".wh.") {
                if basename == ".wh..wh..opq" {
                    // Opaque whiteout: delete all children of the parent dir.
                    let parent = parent_str(&path_str);
                    self.unlink(parent, true)?;
                } else {
                    // Single-file whiteout: `.wh.<name>` deletes `<name>`.
                    let target_name = &basename[".wh.".len()..];
                    let parent = parent_str(&path_str);
                    let target = if parent == "/" {
                        format!("/{target_name}")
                    } else {
                        format!("{parent}/{target_name}")
                    };
                    self.unlink(&target, false)?;
                }
                continue;
            }

            // ── Hard links (deferred) ──
            if let Some(link_target) = entry.link_name().map_err(io_to_format)? {
                let target_str = preprocess_path(&link_target.into_owned());
                hardlinks.insert(
                    PathBuf::from(&path_str),
                    PathBuf::from(target_str),
                );
                continue;
            }

            // ── Timestamps ──
            let ts = entry_timestamps(&entry);

            // ── uid / gid ──
            let header = entry.header();
            let uid = header.uid().ok().map(|u| u as u32);
            let gid = header.gid().ok().map(|g| g as u32);
            let perm = (header.mode().unwrap_or(0o644) & 0o7777) as u16;

            match entry.header().entry_type() {
                tar::EntryType::Directory => {
                    self.create(
                        &path_str,
                        make_mode(file_mode::S_IFDIR, perm),
                        None,
                        Some(ts),
                        None,
                        uid,
                        gid,
                        None,
                    )?;
                }
                tar::EntryType::Regular | tar::EntryType::Continuous => {
                    self.create(
                        &path_str,
                        make_mode(file_mode::S_IFREG, perm),
                        None,
                        Some(ts),
                        Some(&mut entry as &mut dyn Read),
                        uid,
                        gid,
                        None,
                    )?;
                }
                tar::EntryType::Symlink => {
                    let target = entry
                        .link_name()
                        .map_err(io_to_format)?
                        .map(|p| p.to_string_lossy().into_owned());
                    self.create(
                        &path_str,
                        make_mode(file_mode::S_IFLNK, perm),
                        target.as_deref(),
                        Some(ts),
                        None,
                        uid,
                        gid,
                        None,
                    )?;
                }
                // Block/char devices, FIFOs, sockets -- silently skip.
                _ => continue,
            }
        }

        // ── Resolve hard links ──
        if !check_acyclic(&hardlinks) {
            return Err(FormatError::CircularLinks);
        }

        for (link_path, _) in &hardlinks {
            if let Some(resolved) = resolve_hardlink(link_path, &hardlinks) {
                let link_str = link_path.to_string_lossy();
                let target_str = resolved.to_string_lossy();
                self.link(&link_str, &target_str)?;
            }
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Normalize a tar entry path into an absolute path starting with "/".
fn preprocess_path(p: &Path) -> String {
    let s = p.to_string_lossy();
    let mut s = s.as_ref();

    // Strip leading "./"
    if let Some(stripped) = s.strip_prefix("./") {
        s = stripped;
    }

    // Ensure leading "/"
    if !s.starts_with('/') {
        return format!("/{s}");
    }
    s.to_string()
}

/// Return the parent directory of a path string. "/" -> "/"
fn parent_str(path: &str) -> &str {
    if path == "/" {
        return "/";
    }
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(0) => "/",
        Some(i) => &trimmed[..i],
        None => "/",
    }
}

/// Build `FileTimestamps` from a tar entry's header.
fn entry_timestamps<R: Read>(entry: &tar::Entry<'_, R>) -> FileTimestamps {
    let (now_lo, now_hi) = timestamp_now();

    let mtime = entry.header().mtime().unwrap_or(0);
    let mtime_lo = mtime as u32;

    FileTimestamps {
        access_lo: mtime_lo,
        access_hi: 0,
        modification_lo: mtime_lo,
        modification_hi: 0,
        creation_lo: mtime_lo,
        creation_hi: 0,
        now_lo,
        now_hi,
    }
}

/// Check that the hard-link map contains no cycles.
fn check_acyclic(links: &HashMap<PathBuf, PathBuf>) -> bool {
    for target in links.values() {
        let mut visited = std::collections::HashSet::new();
        visited.insert(target.clone());
        let mut next = target.clone();
        while let Some(item) = links.get(&next) {
            if visited.contains(item) {
                return false;
            }
            visited.insert(item.clone());
            next = item.clone();
        }
    }
    true
}

/// Resolve a hard-link chain to its final target path.
fn resolve_hardlink(
    key: &Path,
    links: &HashMap<PathBuf, PathBuf>,
) -> Option<PathBuf> {
    let target = links.get(key)?;
    let mut next = target.clone();
    let mut visited = std::collections::HashSet::new();
    visited.insert(next.clone());
    while let Some(item) = links.get(&next) {
        if visited.contains(item) {
            return None; // cycle
        }
        visited.insert(item.clone());
        next = item.clone();
    }
    Some(next)
}

fn io_to_format(e: std::io::Error) -> FormatError {
    FormatError::Io(e)
}
