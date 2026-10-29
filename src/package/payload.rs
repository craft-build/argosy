//! Payload assembly: the file walk behind packaging, its filters, and the sidecar text.

use std::fs;
use std::path::{Component, Path, PathBuf};

use snafu::ResultExt;

use crate::bundle::sorted_walk;
use crate::error::{Error, IoSnafu, Result, SymlinkEscapeSnafu};
use crate::hash::sha256_hex;

use super::INTEGRITY_FILENAME;
/// The root namespace that never leaves the source.
pub(super) const MEMORY_DIR: &str = "memory";
/// The derivative index directory.
pub(super) const INDEX_DIR: &str = ".argosy";

/// The bundle-relative file paths [`package`] copies: everything except
/// `memory/` (first component only, so nested lookalikes survive) and
/// `.argosy/` unless `include_index`.
///
/// Unreadable directories are a hard error, never a silent drop.
pub(super) fn walk_copied_files(root: &Path, include_index: bool) -> Result<Vec<PathBuf>> {
    let walk = sorted_walk(root, Path::new(""));
    if let Some((rel, source)) = walk.unreadable.into_iter().next() {
        return Err(Error::Io {
            path: root.join(rel),
            source,
        });
    }
    let mut files: Vec<PathBuf> = walk
        .entries
        .iter()
        .filter(|e| !e.is_dir)
        .map(|e| e.rel.clone())
        .filter(|rel| {
            rel.components().next() != Some(Component::Normal(std::ffi::OsStr::new(MEMORY_DIR)))
                // A root-level sidecar from a previous run is regenerated
                // content: excluding it keeps the hash stable across a copy.
                && rel != Path::new(INTEGRITY_FILENAME)
        })
        .collect();

    if include_index {
        let index_root = root.join(INDEX_DIR);
        if index_root.is_dir() {
            collect_files(&index_root, Path::new(INDEX_DIR), &mut files)?;
        }
        files.sort();
    }
    Ok(files)
}

/// Recursively collects every regular file under `root.join(rel)` (used for `.argosy/`).
pub(super) fn collect_files(root: &Path, rel: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let mut entries: Vec<fs::DirEntry> = fs::read_dir(root)
        .context(IoSnafu {
            path: root.to_path_buf(),
        })?
        .collect::<std::io::Result<Vec<_>>>()
        .context(IoSnafu {
            path: root.to_path_buf(),
        })?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let rel = rel.join(entry.file_name());
        let ty = entry.file_type().context(IoSnafu { path: rel.clone() })?;
        if ty.is_dir() {
            collect_files(&entry.path(), &rel, out)?;
        } else if ty.is_file() || ty.is_symlink() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            // SQLite WAL/SHM sidecars never enter the payload: the index is
            // checkpointed first, so the main file alone carries full state.
            if name.ends_with("-wal") || name.ends_with("-shm") {
                continue;
            }
            out.push(rel);
        }
    }
    Ok(())
}

/// Reads `root/rel`, resolving symlinks: a link is materialized as its
/// target's contents iff the canonical target stays within the canonical
/// bundle root, else packaging refuses.
///
/// A residual TOCTOU race is accepted against a caller-controlled workspace.
pub(super) fn read_bundle_file(
    root: &Path,
    canonical_root: &Path,
    rel: &Path,
    include_index: bool,
) -> Result<Vec<u8>> {
    let path = root.join(rel);
    // Canonicalize once and read the resolved path: a later symlink swap cannot redirect it.
    let target = fs::canonicalize(&path).context(IoSnafu { path: path.clone() })?;
    if !target.starts_with(canonical_root) {
        return SymlinkEscapeSnafu { path: path.clone() }.fail();
    }
    // A symlinked alias must not land inside an excluded namespace: the walk filter is name-based.
    if target != canonical_root.join(rel)
        && let Some(first) = target
            .strip_prefix(canonical_root)
            .ok()
            .and_then(|inner| inner.components().next())
    {
        let aliases_excluded = first == Component::Normal(std::ffi::OsStr::new(MEMORY_DIR))
            || (!include_index && first == Component::Normal(std::ffi::OsStr::new(INDEX_DIR)));
        if aliases_excluded {
            return SymlinkEscapeSnafu { path }.fail();
        }
    }
    fs::read(&target).context(IoSnafu { path: target })
}

/// Bundle-relative path rendered with `/` separators, for sidecar lines and
/// tar entry names that must read identically on any platform.
pub(super) fn posix(rel: &Path) -> String {
    rel.to_string_lossy().replace('\\', "/")
}

/// Reads and hashes every file to be packaged in one pass, manifest first,
/// remaining paths sorted.
pub(super) fn collect_payload(
    root: &Path,
    include_index: bool,
) -> Result<Vec<(PathBuf, Vec<u8>, String)>> {
    let canonical_root = fs::canonicalize(root).context(IoSnafu {
        path: root.to_path_buf(),
    })?;
    let mut payload: Vec<(PathBuf, Vec<u8>, String)> = walk_copied_files(root, include_index)?
        .into_iter()
        .map(|rel| {
            let bytes = read_bundle_file(root, &canonical_root, &rel, include_index)?;
            let hash = sha256_hex(&bytes);
            Ok((rel, bytes, hash))
        })
        .collect::<Result<_>>()?;
    payload.sort_by(|a, b| {
        let manifest_a = a.0 == Path::new("argosy.md");
        let manifest_b = b.0 == Path::new("argosy.md");
        manifest_b.cmp(&manifest_a).then_with(|| a.0.cmp(&b.0))
    });
    Ok(payload)
}

/// The sidecar text: one `sha256  <relative-path>` line per payload file.
pub(super) fn integrity_text(payload: &[(PathBuf, Vec<u8>, String)]) -> String {
    let mut out = String::new();
    for (rel, _, hash) in payload {
        out.push_str(&format!("{hash}  {}\n", posix(rel)));
    }
    out
}

/// A sibling path next to `dest` for failure-atomic staging: the artifact is
/// renamed over `dest` only on success.
pub(super) fn staging_path(dest: &Path) -> PathBuf {
    let mut name = dest
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(format!(".tmp-{}", std::process::id()));
    dest.with_file_name(name)
}
