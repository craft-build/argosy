//! Deterministic directory walks that never follow symlinks.

use std::fs;
use std::path::{Path, PathBuf};

/// A bundle's directory entry, with its bundle-root-relative path.
pub(crate) struct WalkEntry {
    pub(crate) rel: PathBuf,
    pub(crate) is_dir: bool,
}

/// The outcome of a recursive walk: the entries found, plus every directory
/// that could not be read. Per-directory read failures are collected rather
/// than aborting the walk, so validation can report them precisely (with the
/// offending path) and still run its other checks.
#[derive(Default)]
pub(crate) struct WalkResult {
    pub(crate) entries: Vec<WalkEntry>,
    /// `(relative path, source)` of failed reads. An empty relative path
    /// means the walk root itself could not be read.
    pub(crate) unreadable: Vec<(PathBuf, std::io::Error)>,
}

/// Recursively collects every entry under `root.join(rel)`. `.argosy/` index
/// directories are skipped entirely: the index is a derivative artifact, not
/// bundle content. Directory symlinks are not followed
/// (`file_type`, not `metadata`), so cycles are impossible.
pub(crate) fn walk_bundle(root: &Path, rel: &Path, walk: &mut WalkResult) {
    let rd = match fs::read_dir(root.join(rel)) {
        Ok(rd) => rd,
        Err(e) => {
            walk.unreadable.push((rel.to_path_buf(), e));
            return;
        }
    };
    let mut entries: Vec<fs::DirEntry> = rd
        .filter_map(|entry| match entry {
            Ok(entry) => Some(entry),
            Err(e) => {
                walk.unreadable.push((rel.to_path_buf(), e));
                None
            }
        })
        .collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let name = entry.file_name();
        let rel = rel.join(&name);
        let is_dir = match entry.file_type() {
            Ok(ty) => ty.is_dir(),
            Err(e) => {
                walk.unreadable.push((rel, e));
                continue;
            }
        };
        if is_dir && name == ".argosy" {
            continue;
        }
        walk.entries.push(WalkEntry {
            rel: rel.clone(),
            is_dir,
        });
        if is_dir {
            walk_bundle(root, &rel, walk);
        }
    }
}

/// Walks `root.join(rel)`, returning entries and read failures both sorted
/// deterministically by relative path.
pub(crate) fn sorted_walk(root: &Path, rel: &Path) -> WalkResult {
    let mut walk = WalkResult::default();
    walk_bundle(root, rel, &mut walk);
    walk.entries.sort_by(|a, b| a.rel.cmp(&b.rel));
    walk.unreadable.sort_by(|a, b| a.0.cmp(&b.0));
    walk
}
