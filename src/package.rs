//! Distribution packaging, bundle integrity, and Craft YAML styleguide
//! import (spec §8 `DIST-1`–`DIST-6`, §7.6 `IDX-14`–`IDX-16`, `MEM-3`).
//!
//! **Out.** [`package`] copies an open [`Argosy`] to a destination directory
//! or a gzipped tarball — the markdown bundle itself is the distributable
//! artifact (`DIST-1`), so git-distribution needs no code here (`DIST-2`
//! stays mechanism-agnostic by design). Two things never or only
//! conditionally ride along:
//!
//! - `memory/` is excluded unconditionally (`DIST-3`/`MEM-3`) by filtering
//!   the walk on the *first* relative path component — a structural rule,
//!   not a glob, so nested lookalikes such as `document/memory-notes/` (or
//!   even `document/memory/`) are ordinary content and survive. There is no
//!   override flag, and when a root `memory/` existed at packaging time the
//!   report carries a `DIST-4` warning so the exclusion is visible even when
//!   it worked.
//! - `.argosy/` ships only under [`PackageOptions::include_index`] (`IDX-14`),
//!   and then purely as a precomputed cache — consumers must still treat it
//!   as derivative (`IDX-16`) and reconcile against the markdown.
//!
//! **Integrity** (`DIST-6`). Every copy emits [`INTEGRITY_FILENAME`], a
//! SHA256SUMS-style sidecar (`<sha256>  <relative-path>` per line, manifest
//! first, remaining paths sorted) inside the destination or archive root;
//! [`validate_integrity`] recomputes it, and [`bundle_content_hash`] reduces
//! the whole distributable content to one digest for change detection that
//! does not depend on `argosy_version` bumps. Because the content hash covers
//! exactly the files packaging copies, it is stable across a packaged copy.
//!
//! **Symlinks** are never preserved as links. A link resolving inside the
//! bundle is materialized as the target file's contents; one whose canonical
//! target escapes the bundle root fails packaging with
//! [`Error::SymlinkEscape`].
//!
//! **In.** [`import_styleguide_yaml`] performs the one-time conversion the
//! reference doc (§4, §6) prescribes for Craft's YAML rule sets: one
//! `Styleguide Rule` concept per rule at
//! `styleguide/<language or "general">/<category or "misc">/<RULE-ID>.md`,
//! written through [`LocalArgosy::write_concept`] so the `STG-2`/`STG-3`
//! contract is enforced by the same code path as every other write. The
//! batch is partial-failure tolerant: a rule that cannot be converted or
//! written becomes a [`Finding`] in the report, not an abort.

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;

use sha2::{Digest, Sha256};
use snafu::ResultExt;
use yaml_serde::{Mapping, Value};

use crate::bundle::{Argosy, Finding, Severity, sorted_walk};
use crate::concept::{Concept, ConceptId};
use crate::error::{
    Error, IntegrityMismatchSnafu, IoSnafu, NotAnArgosySnafu, Result, SymlinkEscapeSnafu,
    ValidationSnafu,
};
use crate::local::LocalArgosy;

/// The integrity sidecar every package emits (`DIST-6`).
pub const INTEGRITY_FILENAME: &str = "argosy-integrity.txt";
/// The root namespace that never leaves the source (`DIST-3`/`MEM-3`).
const MEMORY_DIR: &str = "memory";
/// The derivative index directory (`IDX-14`).
const INDEX_DIR: &str = ".argosy";

/// How [`package`] materializes the distributable bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PackageFormat {
    /// A plain directory tree — the artifact you would commit to git.
    #[default]
    Directory,
    /// A gzipped tar archive (`.tar.gz`).
    TarGz,
}

/// Knobs for [`package`].
#[derive(Debug, Clone, Default)]
pub struct PackageOptions {
    /// Ship `.argosy/` as a precomputed index cache (`IDX-14`). Off by
    /// default: the index is derivative (`IDX-16`) and rebuildable, so most
    /// distributions leave it out.
    pub include_index: bool,
    /// The materialization format.
    pub format: PackageFormat,
}

/// The outcome of a [`package`] run.
#[derive(Debug, Clone)]
pub struct PackageReport {
    /// Manifest name of the packaged argosy (`DIST-5`: the CLI prints
    /// "packaged <name> <version>").
    pub name: String,
    /// Manifest `argosy_version` of the packaged argosy.
    pub argosy_version: semver::Version,
    /// Bundle files copied (excluding the integrity sidecar).
    pub files_copied: usize,
    /// True iff a root `memory/` directory existed at the source and was
    /// excluded (`DIST-3`/`MEM-3`).
    pub memory_excluded: bool,
    /// Non-fatal observations; always contains the `DIST-4` warning when
    /// [`PackageReport::memory_excluded`] is true.
    pub warnings: Vec<String>,
}

/// The outcome of an [`import_styleguide_yaml`] run.
#[derive(Debug, Clone, Default)]
pub struct ImportReport {
    /// Rule concepts successfully written this run.
    pub written: usize,
    /// Rule ids whose target concept already existed and were therefore left
    /// untouched — imports are additive and re-runnable; silently overwriting
    /// user edits is how you lose trust (`STG-8`'s user-extensibility spirit).
    pub skipped_existing: Vec<String>,
    /// Rules that could not be converted or written. The batch never aborts
    /// on one bad rule; callers (doc 09's command) treat a non-empty vec as
    /// failure after the fact.
    pub findings: Vec<Finding>,
}

/// The bundle-relative file paths [`package`] copies: every file under the
/// root except the `memory/` namespace (first-component filter — nested
/// lookalikes survive) and `.argosy/` unless `include_index`. Hard-errors on
/// unreadable directories: packaging must never silently drop content the
/// read-only validator would only have reported.
fn walk_copied_files(root: &Path, include_index: bool) -> Result<Vec<PathBuf>> {
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
                // A root-level sidecar from a previous packaging run is not
                // content: it is regenerated, so it never enters the payload
                // (and therefore never the content hash either — that is what
                // makes `bundle_content_hash` stable across a packaged copy).
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

/// Recursively collects every regular file under `root.join(rel)`, appending
/// bundle-relative paths (used for `.argosy/`, which the bundle walker skips
/// by design since the index is not bundle content).
fn collect_files(root: &Path, rel: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
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
            out.push(rel);
        }
    }
    Ok(())
}

/// Reads `root/rel` into bytes, resolving symlinks along the way. A symlink
/// is materialized as its target's contents iff the canonical target stays
/// within the (canonical) bundle root; otherwise packaging refuses: silently
/// following an escaping link would smuggle outside content into the
/// distributable artifact, which `DIST-3`'s containment rule forbids.
///
/// Known residual race: the containment check and the read are separate
/// syscalls (TOCTOU), so a concurrently-mutating actor could swap the link
/// in between. Acceptable at this layer — packaging runs against a workspace
/// the caller controls; fd-relative reads would be needed to close it fully.
fn read_bundle_file(
    root: &Path,
    canonical_root: &Path,
    rel: &Path,
    include_index: bool,
) -> Result<Vec<u8>> {
    let path = root.join(rel);
    // Canonicalize unconditionally and read the *resolved* path: check and
    // read share one resolution, so a symlink swap afterwards cannot
    // redirect the read.
    let target = fs::canonicalize(&path).context(IoSnafu { path: path.clone() })?;
    if !target.starts_with(canonical_root) {
        return SymlinkEscapeSnafu { path: path.clone() }.fail();
    }
    // If resolution crossed a symlink, the target must not land inside an
    // excluded namespace: the name-based walk filter cannot see aliases.
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

/// The lowercase hex SHA-256 of `data`.
fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex(&hasher.finalize())
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Bundle-relative path rendered with `/` separators, for sidecar lines and
/// tar entry names that must read identically on any platform.
fn posix(rel: &Path) -> String {
    rel.to_string_lossy().replace('\\', "/")
}

/// Reads and hashes every file about to be packaged in one pass, so the
/// copy loop, the tar appender, and the integrity sidecar all share one
/// digest computation. The manifest is ordered first, remaining paths
/// sorted (`DIST-6` sidecar layout).
///
/// Tradeoff: the whole payload — including a shipped `.argosy/` index
/// database — is resident in memory at once. Bundles are knowledge bases,
/// so this is comfortably small at expected scales; if that ever stops
/// being true, copy-with-streaming-hash plus a sidecar-last second pass
/// would bound memory at one file.
fn collect_payload(root: &Path, include_index: bool) -> Result<Vec<(PathBuf, Vec<u8>, String)>> {
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
fn integrity_text(payload: &[(PathBuf, Vec<u8>, String)]) -> String {
    let mut out = String::new();
    for (rel, _, hash) in payload {
        out.push_str(&format!("{hash}  {}\n", posix(rel)));
    }
    out
}

/// A sibling path next to `dest` used for failure-atomic staging: the full
/// artifact is materialized there first and renamed over `dest` only on
/// success, so a failed run never leaves a partial bundle or a truncated
/// archive where a valid one (or nothing) stood.
fn staging_path(dest: &Path) -> PathBuf {
    let mut name = dest
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(format!(".tmp-{}", std::process::id()));
    dest.with_file_name(name)
}

/// Copies `source` to `dest` for distribution (`DIST-1`).
///
/// `dest` must not already contain anything for [`PackageFormat::Directory`]
/// (packaging never merges into or overwrites an existing tree; the check
/// happens before any copying starts). Both formats are staged to a sibling
/// temp path and renamed over `dest` on success, so a mid-copy failure
/// leaves neither a partial tree nor a truncated archive. `dest` must lie
/// outside the source bundle. The integrity sidecar is written last, inside
/// the directory or archive root.
pub fn package(source: &Argosy, dest: &Path, options: &PackageOptions) -> Result<PackageReport> {
    let root = source.root();

    // Refuse to package a bundle into itself: Directory mode would mutate
    // the source underfoot, TarGz would truncate a payload file that was
    // part of the copy set.
    let canonical_root = fs::canonicalize(root).context(IoSnafu {
        path: root.to_path_buf(),
    })?;
    let canonical_dest = if dest.exists() {
        fs::canonicalize(dest).context(IoSnafu {
            path: dest.to_path_buf(),
        })?
    } else {
        let parent = dest
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let parent = fs::canonicalize(parent).context(IoSnafu {
            path: parent.to_path_buf(),
        })?;
        match dest.file_name() {
            Some(name) => parent.join(name),
            None => parent,
        }
    };
    if canonical_dest.starts_with(&canonical_root) {
        return ValidationSnafu {
            reason: format!(
                "packaging destination `{}` lies inside the source bundle `{}`",
                dest.display(),
                root.display()
            ),
        }
        .fail();
    }

    // Probe before copying: DIST-4 wants the exclusion *visible* whenever
    // memory/ existed at packaging time, even though it always works.
    let memory_excluded = fs::symlink_metadata(root.join(MEMORY_DIR)).is_ok();
    let payload = collect_payload(root, options.include_index)?;
    let integrity = integrity_text(&payload);

    match options.format {
        PackageFormat::Directory => {
            if dest.exists()
                && (!dest.is_dir()
                    || fs::read_dir(dest)
                        .context(IoSnafu {
                            path: dest.to_path_buf(),
                        })?
                        .next()
                        .is_some())
            {
                return ValidationSnafu {
                    reason: format!(
                        "packaging destination `{}` already exists and is not an empty directory",
                        dest.display()
                    ),
                }
                .fail();
            }
            let staging = staging_path(dest);
            // A crashed earlier run may have left a stale staging tree behind
            // (PID reuse makes the name collide); never merge into it.
            let _ = fs::remove_dir_all(&staging);
            let result = (|staging: &Path| -> Result<()> {
                fs::create_dir_all(staging).context(IoSnafu {
                    path: staging.to_path_buf(),
                })?;
                for (rel, bytes, _) in &payload {
                    let out = staging.join(rel);
                    if let Some(parent) = out.parent() {
                        fs::create_dir_all(parent).context(IoSnafu {
                            path: parent.to_path_buf(),
                        })?;
                    }
                    fs::write(&out, bytes).context(IoSnafu { path: out })?;
                }
                fs::write(staging.join(INTEGRITY_FILENAME), &integrity).context(IoSnafu {
                    path: staging.join(INTEGRITY_FILENAME),
                })?;
                Ok(())
            })(&staging);
            if let Err(e) = result {
                let _ = fs::remove_dir_all(&staging);
                return Err(e);
            }
            // Commit: swap the staged tree into place. On failure, clean up
            // the staging tree too — a full payload copy must not linger.
            let result = (|staging: &Path| -> Result<()> {
                if dest.exists() {
                    // Verified empty above; remove so the rename can take over.
                    fs::remove_dir(dest).context(IoSnafu {
                        path: dest.to_path_buf(),
                    })?;
                }
                fs::rename(staging, dest).context(IoSnafu {
                    path: dest.to_path_buf(),
                })?;
                Ok(())
            })(&staging);
            if let Err(e) = result {
                let _ = fs::remove_dir_all(&staging);
                return Err(e);
            }
        }
        PackageFormat::TarGz => {
            let staging = staging_path(dest);
            let result = (|staging: &Path| -> Result<()> {
                let file = fs::File::create(staging).context(IoSnafu {
                    path: staging.to_path_buf(),
                })?;
                let enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
                let mut builder = tar::Builder::new(enc);
                for (rel, bytes, _) in &payload {
                    let mut header = tar::Header::new_gnu();
                    header.set_size(bytes.len() as u64);
                    header.set_mode(0o644);
                    header.set_cksum();
                    builder
                        .append_data(&mut header, posix(rel), bytes.as_slice())
                        .context(IoSnafu {
                            path: root.join(rel),
                        })?;
                }
                let mut header = tar::Header::new_gnu();
                header.set_size(integrity.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder
                    .append_data(&mut header, INTEGRITY_FILENAME, integrity.as_bytes())
                    .context(IoSnafu {
                        path: staging.to_path_buf(),
                    })?;
                builder.finish().context(IoSnafu {
                    path: staging.to_path_buf(),
                })?;
                // `finish()` flushes the gzip stream; the `File` then closes on drop.
                let _file = builder
                    .into_inner()
                    .and_then(|enc| enc.finish())
                    .context(IoSnafu {
                        path: staging.to_path_buf(),
                    })?;
                Ok(())
            })(&staging);
            if let Err(e) = result {
                let _ = fs::remove_file(&staging);
                return Err(e);
            }
            // Commit with the same cleanup discipline as the copy path.
            let result = fs::rename(&staging, dest).context(IoSnafu {
                path: dest.to_path_buf(),
            });
            if let Err(e) = result {
                let _ = fs::remove_file(&staging);
                return Err(e);
            }
        }
    }

    let mut warnings = Vec::new();
    if memory_excluded {
        warnings.push(format!("{MEMORY_DIR}/ present and excluded from package"));
    }
    let manifest = source.manifest();
    Ok(PackageReport {
        name: manifest.name().to_string(),
        argosy_version: manifest.argosy_version().clone(),
        files_copied: payload.len(),
        memory_excluded,
        warnings,
    })
}

/// One SHA-256 over the ordered `(relative-path, file-hash)` pairs of
/// everything [`package`] would copy — the spec's recommended change
/// detector (`DIST-6`), independent of `argosy_version` bumps. Because the
/// covered file set matches the packaging copy set, the hash is stable
/// across a packaged copy and changes when any distributable file does.
///
/// Errors when `argosy_root` is not a bundle root (no `argosy.md`).
pub fn bundle_content_hash(argosy_root: &Path) -> Result<String> {
    if !argosy_root.join("argosy.md").is_file() {
        return NotAnArgosySnafu {
            path: argosy_root.to_path_buf(),
            reason: "no `argosy.md` manifest at the root".to_string(),
        }
        .fail();
    }
    let payload = collect_payload(argosy_root, false)?;
    let mut hasher = Sha256::new();
    for (rel, _, hash) in &payload {
        hasher.update(posix(rel).as_bytes());
        hasher.update(b"\n");
        hasher.update(hash.as_bytes());
        hasher.update(b"\n");
    }
    Ok(hex(&hasher.finalize()))
}

/// Recomputes every hash in the bundle's integrity sidecar (`DIST-6`). A
/// missing sidecar, a line whose file is absent, or a hash that disagrees is
/// an [`Error::IntegrityMismatch`], as is a bundle file present on disk but
/// not listed in the sidecar — the sidecar claims completeness.
pub fn validate_integrity(argosy_root: &Path) -> Result<()> {
    let sidecar_path = argosy_root.join(INTEGRITY_FILENAME);
    let text = match fs::read_to_string(&sidecar_path) {
        Ok(text) => text,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return IntegrityMismatchSnafu {
                path: sidecar_path.clone(),
                reason: "integrity sidecar missing".to_string(),
            }
            .fail();
        }
        Err(source) => {
            return Err(Error::Io {
                path: sidecar_path,
                source,
            });
        }
    };
    let canonical_root = fs::canonicalize(argosy_root).context(IoSnafu {
        path: argosy_root.to_path_buf(),
    })?;
    let mut listed: Vec<PathBuf> = Vec::new();
    for line in text.lines() {
        let Some((expected, rel)) = line.split_once("  ") else {
            return IntegrityMismatchSnafu {
                path: sidecar_path.clone(),
                reason: format!("malformed sidecar line `{line}`"),
            }
            .fail();
        };
        // The sidecar text is attacker-controlled: reject anything but
        // plain non-empty relative paths before touching the filesystem
        // (`DIST-3` containment — this covers literal `..` components, while
        // `read_bundle_file`'s unconditional canonicalize covers symlinked
        // path components).
        if rel.is_empty()
            || Path::new(rel)
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
        {
            return IntegrityMismatchSnafu {
                path: sidecar_path.clone(),
                reason: format!("sidecar path `{rel}` escapes the bundle root"),
            }
            .fail();
        }
        let rel = PathBuf::from(rel);
        let actual = sha256_hex(
            &read_bundle_file(
                argosy_root,
                &canonical_root,
                &rel,
                argosy_root.join(INDEX_DIR).is_dir(),
            )
            .map_err(|e| match e {
                Error::Io { path, source } if source.kind() == std::io::ErrorKind::NotFound => {
                    IntegrityMismatchSnafu {
                        path,
                        reason: "file listed in sidecar is missing".to_string(),
                    }
                    .build()
                }
                other => other,
            })?,
        );
        if actual != expected {
            return IntegrityMismatchSnafu {
                path: argosy_root.join(&rel),
                reason: "content hash does not match sidecar".to_string(),
            }
            .fail();
        }
        listed.push(rel);
    }
    listed.sort();
    // The writer lists `.argosy/**` when it shipped them (`IDX-14`), so
    // completeness is checked against whatever is actually on disk.
    let actual = walk_copied_files(argosy_root, argosy_root.join(INDEX_DIR).is_dir())?;
    if listed != actual {
        return IntegrityMismatchSnafu {
            path: sidecar_path,
            reason: "sidecar file list does not match bundle contents".to_string(),
        }
        .fail();
    }
    Ok(())
}

/// Imports Craft YAML rule sets into the local argosy's `styleguide/`
/// namespace (reference doc §4, §6).
///
/// `yaml_dir` holds `*.yaml`/`*.yml` files, each decoding to either a
/// sequence of rule objects or a mapping with a top-level `rules:` sequence.
/// A rule object needs `id` and `description`; `language`, `category`,
/// `priority` (`error`/`warn`/`info`), `pattern`, and `good`/`bad` examples
/// (a string or a list of strings) are optional, and extra keys are ignored.
/// Each rule becomes one concept at
/// `styleguide/<language or "general">/<category or "misc">/<RULE-ID>.md`
/// with `type: Styleguide Rule` frontmatter and, when examples exist,
/// `## Good`/`## Bad` body sections (`STG-6`).
///
/// Writes go through [`LocalArgosy::write_concept`], so `STG-2`/`STG-3` are
/// enforced by the one shared write path. A rule or file that fails reading,
/// conversion, or validation is collected into [`ImportReport::findings`]
/// without aborting the batch; an already-existing target goes to
/// [`ImportReport::skipped_existing`] untouched (imports are additive and
/// re-runnable).
pub fn import_styleguide_yaml(local: &LocalArgosy, yaml_dir: &Path) -> Result<ImportReport> {
    let mut report = ImportReport::default();
    let mut entries: Vec<fs::DirEntry> = fs::read_dir(yaml_dir)
        .context(IoSnafu {
            path: yaml_dir.to_path_buf(),
        })?
        .collect::<std::io::Result<Vec<_>>>()
        .context(IoSnafu {
            path: yaml_dir.to_path_buf(),
        })?;
    entries.sort_by_key(std::fs::DirEntry::file_name);

    for entry in entries {
        let path = entry.path();
        let is_yaml = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e == "yaml" || e == "yml");
        if !path.is_file() || !is_yaml {
            continue;
        }
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) => {
                report.findings.push(Finding::new(
                    Severity::Error,
                    None,
                    Some(path.clone()),
                    format!("failed to read file: {e}"),
                ));
                continue;
            }
        };
        let value: Value = match yaml_serde::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                report.findings.push(Finding::new(
                    Severity::Error,
                    None,
                    Some(path.clone()),
                    format!("failed to parse YAML: {e}"),
                ));
                continue;
            }
        };
        import_rule_file(local, &value, &path, &mut report);
    }
    Ok(report)
}

/// Converts every rule object in one decoded YAML file. Accepts a bare
/// sequence or a mapping with a `rules:` sequence (the two observed Craft
/// shapes); anything else is one file-level finding.
fn import_rule_file(local: &LocalArgosy, value: &Value, file: &Path, report: &mut ImportReport) {
    let rules = match value {
        Value::Sequence(items) => Some(items.clone()),
        Value::Mapping(map) => match map.get("rules") {
            Some(Value::Sequence(items)) => Some(items.clone()),
            _ => None,
        },
        _ => None,
    };
    let Some(rules) = rules else {
        report.findings.push(Finding::new(
            Severity::Error,
            None,
            Some(file.to_path_buf()),
            "expected a sequence of rules or a mapping with a top-level `rules:` key",
        ));
        return;
    };
    for item in rules {
        let Value::Mapping(rule) = item else {
            report.findings.push(Finding::new(
                Severity::Error,
                None,
                Some(file.to_path_buf()),
                "rule entry is not a mapping",
            ));
            continue;
        };
        import_one_rule(local, &rule, file, report);
    }
}

/// Converts one decoded rule object: `Err(String)` carries the human-readable
/// reason the rule cannot become a finding-free concept.
fn rule_to_concept(rule: &Mapping) -> std::result::Result<(String, Concept), String> {
    let get_str = |key: &str| rule.get(key).and_then(Value::as_str);
    let id = get_str("id")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "rule has no `id`".to_string())?;
    // Examples preserve the YAML form: a sequence renders as `- ` bullets, a
    // bare string verbatim (`STG-6`). Non-string items inside a sequence are
    // reported, matching the scalar-field strictness below — silently
    // dropping `42` from `good: ["ok", 42]` loses rule content invisibly.
    let get_examples = |key: &str| -> std::result::Result<(Vec<String>, bool), String> {
        match rule.get(key) {
            Some(Value::Sequence(items)) => {
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    match item.as_str() {
                        Some(s) => out.push(s.to_string()),
                        None => {
                            return Err(format!("rule `{id}`: `{key}` list items must be strings"));
                        }
                    }
                }
                Ok((out, true))
            }
            Some(Value::String(s)) => Ok((vec![s.clone()], false)),
            _ => Ok((Vec::new(), false)),
        }
    };
    let description = get_str("description")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("rule `{id}`: missing required `description`"))?;
    // Non-string scalars in the string fields would otherwise be silently
    // dropped (`priority: 1` imports without its priority); report them the
    // same way an invalid priority value is.
    for key in ["language", "category", "priority"] {
        if let Some(value) = rule.get(key)
            && !matches!(value, Value::String(_) | Value::Null)
        {
            return Err(format!("rule `{id}`: `{key}` must be a string"));
        }
    }
    let priority = match get_str("priority") {
        Some(p @ ("error" | "warn" | "info")) => Some(p),
        Some(p) => {
            return Err(format!(
                "rule `{id}`: `priority` must be one of error/warn/info, got `{p}`"
            ));
        }
        None => None,
    };
    let pattern = match rule.get("pattern") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Sequence(items)) => {
            let mut parts = Vec::with_capacity(items.len());
            for item in items {
                match item.as_str() {
                    Some(s) => parts.push(s),
                    None => {
                        return Err(format!("rule `{id}`: `pattern` list items must be strings"));
                    }
                }
            }
            if parts.is_empty() {
                None
            } else {
                Some(parts.join("\n"))
            }
        }
        Some(_) => {
            return Err(format!(
                "rule `{id}`: `pattern` must be a string or list of strings"
            ));
        }
    };

    let mut frontmatter = Mapping::new();
    let mut put = |key: &str, value: &str| {
        frontmatter.insert(
            Value::String(key.to_string()),
            Value::String(value.to_string()),
        );
    };
    put("type", crate::styleguide::TYPE);
    put("description", description);
    if let Some(language) = get_str("language") {
        put("language", language);
    }
    if let Some(category) = get_str("category") {
        put("category", category);
    }
    put("rule_id", id);
    if let Some(priority) = priority {
        put("priority", priority);
    }
    if let Some(pattern) = &pattern {
        put("pattern", pattern);
    }

    let good = get_examples("good")?;
    let bad = get_examples("bad")?;
    let mut body = description.to_string();
    for (heading, (examples, bulleted)) in [("## Good", &good), ("## Bad", &bad)] {
        if examples.is_empty() {
            continue;
        }
        body.push_str("\n\n");
        body.push_str(heading);
        body.push_str("\n\n");
        for (i, example) in examples.iter().enumerate() {
            if *bulleted {
                body.push_str("- ");
            }
            body.push_str(example);
            if i + 1 < examples.len() {
                body.push('\n');
            }
        }
    }
    body.push('\n');

    let concept = Concept::new(frontmatter, body)
        .map_err(|e| format!("rule `{id}`: cannot build concept: {e}"))?;
    Ok((id.to_string(), concept))
}

/// Writes one converted rule under `styleguide/` (or records why not).
fn import_one_rule(local: &LocalArgosy, rule: &Mapping, file: &Path, report: &mut ImportReport) {
    let (rule_id, concept) = match rule_to_concept(rule) {
        Ok(ok) => ok,
        Err(reason) => {
            report.findings.push(Finding::new(
                Severity::Error,
                None,
                Some(file.to_path_buf()),
                reason,
            ));
            return;
        }
    };
    let language = concept.get_str("language").unwrap_or("general");
    let category = concept.get_str("category").unwrap_or("misc");
    let target = format!("styleguide/{language}/{category}/{rule_id}");
    let id = match ConceptId::from_str(&target) {
        Ok(id) => id,
        Err(e) => {
            report.findings.push(Finding::new(
                Severity::Error,
                None,
                Some(file.to_path_buf()),
                format!("rule `{rule_id}`: not a valid concept path `{target}`: {e}"),
            ));
            return;
        }
    };
    let rel = id.to_relative_path();
    if local.root().join(&rel).exists() {
        report.skipped_existing.push(rule_id);
        return;
    }
    match local.write_rule(&id, &concept) {
        Ok(_) => report.written += 1,
        Err(e) => report.findings.push(Finding::new(
            Severity::Error,
            None,
            Some(rel),
            format!("rule `{rule_id}`: {e}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::local::LocalArgosy;

    /// `validate_styleguide` returns raw findings; these are the error-severity ones.
    fn error_findings(argosy: &Argosy) -> Vec<Finding> {
        argosy
            .validate_styleguide()
            .into_iter()
            .filter(|f| f.severity == Severity::Error)
            .collect()
    }

    const MANIFEST: &str = "---\ntype: Argosy Manifest\nname: acme-billing\nargosy_version: 0.3.1\n---\n\nThe acme billing knowledge bundle.\n";

    fn write(root: &Path, rel: &str, text: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    /// The full fixture from the doc-08 success criteria: every reserved
    /// namespace, a custom one, a `.argosy/` index, and a nested
    /// `document/memory-notes/` that must NOT be caught by the exclusion.
    fn fixture_argosy(dir: &TempDir) -> PathBuf {
        let root = dir.path().join("fixture");
        write(&root, "argosy.md", MANIFEST);
        write(
            &root,
            "document/design.md",
            "---\ntype: Document\ndescription: Design notes.\n---\n\n# Design\n",
        );
        write(
            &root,
            "document/memory-notes/notes.md",
            "---\ntype: Document\ndescription: About memory, not memory itself.\n---\n\nnotes\n",
        );
        write(
            &root,
            "memory/gotchas.md",
            "---\ntype: Memory\ndescription: Private scratch.\n---\n\ngotcha\n",
        );
        write(
            &root,
            "custom/product/faq.md",
            "---\ntype: Note\ndescription: Producer-owned custom namespace.\n---\n\nfaq\n",
        );
        write(&root, ".argosy/index.db", "sqlite bytes");
        root
    }

    fn import_fixture(dir: &TempDir) -> LocalArgosy {
        let root = dir.path().join("local");
        write(&root, "argosy.md", MANIFEST);
        LocalArgosy::open(&root).unwrap()
    }

    #[test]
    fn package_excludes_root_memory_and_index_but_keeps_lookalikes() {
        let dir = TempDir::new().unwrap();
        let root = fixture_argosy(&dir);
        let dest = dir.path().join("out");
        let argosy = Argosy::open(&root).unwrap();
        let report = package(&argosy, &dest, &PackageOptions::default()).unwrap();

        assert_eq!(report.name, "acme-billing");
        assert_eq!(report.argosy_version.to_string(), "0.3.1");
        assert_eq!(report.files_copied, 4);
        assert!(report.memory_excluded);
        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.contains("memory/") && w.contains("excluded")),
            "DIST-4 warning missing: {:?}",
            report.warnings
        );

        assert!(dest.join("argosy.md").is_file());
        assert!(dest.join("document/design.md").is_file());
        assert!(dest.join("document/memory-notes/notes.md").is_file());
        assert!(dest.join("custom/product/faq.md").is_file());
        assert!(!dest.join("memory").exists());
        assert!(!dest.join(".argosy").exists());
    }

    #[test]
    fn no_memory_means_no_warning() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("fixture");
        write(&root, "argosy.md", MANIFEST);
        let dest = dir.path().join("out");
        let argosy = Argosy::open(&root).unwrap();
        let report = package(&argosy, &dest, &PackageOptions::default()).unwrap();
        assert!(!report.memory_excluded);
        assert!(report.warnings.is_empty());
    }

    #[test]
    fn include_index_ships_the_argosy_cache_dir() {
        let dir = TempDir::new().unwrap();
        let root = fixture_argosy(&dir);
        let dest = dir.path().join("out");
        let argosy = Argosy::open(&root).unwrap();
        let opts = PackageOptions {
            include_index: true,
            format: PackageFormat::Directory,
        };
        let report = package(&argosy, &dest, &opts).unwrap();
        assert!(dest.join(".argosy/index.db").is_file());
        assert!(!dest.join("memory").exists());
        assert_eq!(report.files_copied, 5);
    }

    #[test]
    fn package_refuses_a_destination_inside_the_source() {
        let dir = TempDir::new().unwrap();
        let root = fixture_argosy(&dir);
        let argosy = Argosy::open(&root).unwrap();
        let err = package(&argosy, &root.join("out"), &PackageOptions::default()).unwrap_err();
        assert!(
            err.to_string().contains("inside the source bundle"),
            "unexpected error: {err}"
        );
        assert!(!root.join("out").exists(), "nothing was written");
    }

    #[test]
    fn include_index_packages_validate_their_sidecar() {
        let dir = TempDir::new().unwrap();
        let root = fixture_argosy(&dir);
        let dest = dir.path().join("out");
        let argosy = Argosy::open(&root).unwrap();
        let opts = PackageOptions {
            include_index: true,
            format: PackageFormat::Directory,
        };
        package(&argosy, &dest, &opts).unwrap();
        validate_integrity(&dest).unwrap();

        fs::remove_file(dest.join(".argosy/index.db")).unwrap();
        let err = validate_integrity(&dest).unwrap_err();
        assert!(matches!(err, Error::IntegrityMismatch { .. }), "{err:?}");
    }

    #[test]
    fn package_refuses_a_nonempty_destination() {
        let dir = TempDir::new().unwrap();
        let root = fixture_argosy(&dir);
        let dest = dir.path().join("out");
        fs::create_dir_all(&dest).unwrap();
        fs::write(dest.join("stale.txt"), "leftover").unwrap();
        let argosy = Argosy::open(&root).unwrap();
        let err = package(&argosy, &dest, &PackageOptions::default()).unwrap_err();
        assert!(
            err.to_string().contains("not an empty directory"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn packaged_directory_is_itself_a_conformant_argosy() {
        let dir = TempDir::new().unwrap();
        let root = fixture_argosy(&dir);
        write(
            &root,
            "styleguide/rust/naming/snake-case-vars.md",
            "---\ntype: Styleguide Rule\ndescription: Name variables snake_case.\nlanguage: rust\ncategory: naming\n---\n\nGuidance.\n",
        );
        let dest = dir.path().join("out");
        let argosy = Argosy::open(&root).unwrap();
        package(&argosy, &dest, &PackageOptions::default()).unwrap();

        let packaged = Argosy::open(&dest).unwrap();
        assert!(Argosy::validate(&dest).is_conformant());
        assert_eq!(error_findings(&packaged).len(), 0);
    }

    #[test]
    fn integrity_sidecar_matches_recomputation() {
        let dir = TempDir::new().unwrap();
        let root = fixture_argosy(&dir);
        let dest = dir.path().join("out");
        let argosy = Argosy::open(&root).unwrap();
        package(&argosy, &dest, &PackageOptions::default()).unwrap();

        let sidecar = fs::read_to_string(dest.join(INTEGRITY_FILENAME)).unwrap();
        let lines: Vec<&str> = sidecar.lines().collect();
        assert_eq!(lines.len(), 4);
        assert!(
            lines[0].ends_with("  argosy.md"),
            "manifest first: {lines:?}"
        );
        // Spot-check one file against an independent recomputation.
        let (hash, rel) = lines[1].split_once("  ").unwrap();
        let bytes = fs::read(dest.join(rel)).unwrap();
        assert_eq!(hash, sha256_hex(&bytes));

        validate_integrity(&dest).unwrap();
    }

    #[test]
    fn validate_integrity_flags_tampering() {
        let dir = TempDir::new().unwrap();
        let root = fixture_argosy(&dir);
        let dest = dir.path().join("out");
        let argosy = Argosy::open(&root).unwrap();
        package(&argosy, &dest, &PackageOptions::default()).unwrap();

        fs::write(dest.join("document/design.md"), "tampered").unwrap();
        let err = validate_integrity(&dest).unwrap_err();
        assert!(matches!(err, Error::IntegrityMismatch { .. }), "{err:?}");
    }

    #[test]
    fn validate_integrity_flags_a_missing_file() {
        let dir = TempDir::new().unwrap();
        let root = fixture_argosy(&dir);
        let dest = dir.path().join("out");
        let argosy = Argosy::open(&root).unwrap();
        package(&argosy, &dest, &PackageOptions::default()).unwrap();

        fs::remove_file(dest.join("document/design.md")).unwrap();
        let err = validate_integrity(&dest).unwrap_err();
        assert!(matches!(err, Error::IntegrityMismatch { .. }), "{err:?}");
        assert!(err.to_string().contains("missing"), "{err:?}");
    }

    #[test]
    fn validate_integrity_rejects_escaping_sidecar_paths() {
        let dir = TempDir::new().unwrap();
        let root = fixture_argosy(&dir);
        let dest = dir.path().join("out");
        let argosy = Argosy::open(&root).unwrap();
        package(&argosy, &dest, &PackageOptions::default()).unwrap();

        // An attacker-controlled sidecar must not make the validator hash
        // files outside the bundle (`DIST-3` containment).
        fs::write(dest.join(INTEGRITY_FILENAME), "deadbeef  ../outside.txt\n").unwrap();
        let err = validate_integrity(&dest).unwrap_err();
        assert!(
            err.to_string().contains("escapes the bundle root"),
            "{err:?}"
        );

        // An empty path is equally malformed and must not surface as Io.
        fs::write(dest.join(INTEGRITY_FILENAME), "deadbeef  \n").unwrap();
        let err = validate_integrity(&dest).unwrap_err();
        assert!(matches!(err, Error::IntegrityMismatch { .. }), "{err:?}");
    }

    #[cfg(unix)]
    #[test]
    fn validate_integrity_refuses_symlinked_dirs_pointing_outside() {
        let dir = TempDir::new().unwrap();
        let root = fixture_argosy(&dir);
        let dest = dir.path().join("out");
        let argosy = Argosy::open(&root).unwrap();
        package(&argosy, &dest, &PackageOptions::default()).unwrap();

        // Every component is a Normal name, so the textual check passes — it
        // is the canonicalize in `read_bundle_file` that must catch the
        // symlinked intermediate directory.
        let outside = dir.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret.txt"), "do not read").unwrap();
        std::os::unix::fs::symlink(&outside, dest.join("linkdir")).unwrap();
        fs::write(
            dest.join(INTEGRITY_FILENAME),
            "deadbeef  linkdir/secret.txt\n",
        )
        .unwrap();

        let err = validate_integrity(&dest).unwrap_err();
        assert!(matches!(err, Error::SymlinkEscape { .. }), "{err:?}");
    }

    #[test]
    fn bundle_content_hash_changes_with_content_and_is_stable_across_a_copy() {
        let dir = TempDir::new().unwrap();
        let root = fixture_argosy(&dir);
        let before = bundle_content_hash(&root).unwrap();

        fs::write(
            root.join("document/design.md"),
            "---\ntype: Document\ndescription: Design notes.\n---\n\n# Design v2\n",
        )
        .unwrap();
        let after = bundle_content_hash(&root).unwrap();
        assert_ne!(before, after);

        // Packaging excludes memory/ and .argosy/, so a packaged copy hashes
        // the same as its source — the hash covers distributable content.
        let dest = dir.path().join("out");
        let argosy = Argosy::open(&root).unwrap();
        package(&argosy, &dest, &PackageOptions::default()).unwrap();
        assert_eq!(bundle_content_hash(&dest).unwrap(), after);
    }

    #[test]
    fn bundle_content_hash_rejects_a_non_bundle() {
        let dir = TempDir::new().unwrap();
        let err = bundle_content_hash(dir.path()).unwrap_err();
        assert!(matches!(err, Error::NotAnArgosy { .. }), "{err:?}");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escaping_the_bundle_errors() {
        let dir = TempDir::new().unwrap();
        let root = fixture_argosy(&dir);
        let outside = dir.path().join("secret.txt");
        fs::write(&outside, "do not ship").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("document/leak.md")).unwrap();

        let dest = dir.path().join("out");
        let argosy = Argosy::open(&root).unwrap();
        let err = package(&argosy, &dest, &PackageOptions::default()).unwrap_err();
        assert!(matches!(err, Error::SymlinkEscape { .. }), "{err:?}");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_aliasing_memory_or_the_index_is_rejected() {
        let dir = TempDir::new().unwrap();
        let root = fixture_argosy(&dir);
        std::os::unix::fs::symlink("../memory/gotchas.md", root.join("document/alias.md")).unwrap();

        let dest = dir.path().join("out");
        let argosy = Argosy::open(&root).unwrap();
        let err = package(&argosy, &dest, &PackageOptions::default()).unwrap_err();
        assert!(matches!(err, Error::SymlinkEscape { .. }), "{err:?}");

        fs::remove_file(root.join("document/alias.md")).unwrap();
        std::os::unix::fs::symlink("../.argosy/index.db", root.join("document/idx.md")).unwrap();
        let opts = PackageOptions {
            include_index: false,
            format: PackageFormat::Directory,
        };
        let err = package(&argosy, &dest, &opts).unwrap_err();
        assert!(matches!(err, Error::SymlinkEscape { .. }), "{err:?}");

        // include_index ships the cache, so aliasing into it is legitimate.
        let opts = PackageOptions {
            include_index: true,
            format: PackageFormat::Directory,
        };
        package(&argosy, &dir.path().join("out2"), &opts).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn in_bundle_symlink_is_materialized_as_contents() {
        let dir = TempDir::new().unwrap();
        let root = fixture_argosy(&dir);
        std::os::unix::fs::symlink("../argosy.md", root.join("document/manifest-copy.md")).unwrap();

        let dest = dir.path().join("out");
        let argosy = Argosy::open(&root).unwrap();
        package(&argosy, &dest, &PackageOptions::default()).unwrap();

        let copied = dest.join("document/manifest-copy.md");
        assert!(copied.is_file());
        assert!(!copied.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(
            fs::read(&copied).unwrap(),
            fs::read(dest.join("argosy.md")).unwrap()
        );
    }

    #[test]
    fn targz_round_trips_into_a_conformant_argosy() {
        let dir = TempDir::new().unwrap();
        let root = fixture_argosy(&dir);
        let archive = dir.path().join("out.tar.gz");
        let argosy = Argosy::open(&root).unwrap();
        let opts = PackageOptions {
            include_index: false,
            format: PackageFormat::TarGz,
        };
        package(&argosy, &archive, &opts).unwrap();
        assert!(archive.is_file());

        let extracted = dir.path().join("extracted");
        fs::create_dir_all(&extracted).unwrap();
        let decoder = flate2::read::GzDecoder::new(fs::File::open(&archive).unwrap());
        tar::Archive::new(decoder).unpack(&extracted).unwrap();

        assert!(!extracted.join("memory").exists());
        assert!(extracted.join(INTEGRITY_FILENAME).is_file());
        Argosy::open(&extracted).unwrap();
        assert!(Argosy::validate(&extracted).is_conformant());
        validate_integrity(&extracted).unwrap();
    }

    const RUST_RULES: &str = "\
- id: no-unwrap-in-prod
  description: Do not call unwrap outside tests.
  language: rust
  category: error-handling
  priority: error
  pattern: \".unwrap()\"
  good:
    - \"let value = maybe()?;\"
    - \"let value = maybe.expect('known here');\"
  bad: \"let value = maybe.unwrap();\"
- id: minimal-rule
  description: A rule with only the required fields.
";

    const MAPPING_RULES: &str = "\
rules:
  - id: no-eval
    description: Never evaluate strings as code.
    language: python
    priority: warn
    good: \"ast.literal_eval(text)\"
    bad:
      - \"eval(text)\"
";

    const BROKEN_RULES: &str = "\
- id: no-description-here
  language: rust
- id: \"bad:colon\"
  description: This id cannot become a filename.
- id: bad-priority
  description: Priority outside the error/warn/info vocabulary.
  priority: blocker
- id: numeric-priority
  description: Non-string scalars in string fields are not silently dropped.
  priority: 1
";

    #[test]
    fn import_converts_craft_rulesets_into_styleguide_concepts() {
        let dir = TempDir::new().unwrap();
        let local = import_fixture(&dir);
        let yaml_dir = dir.path().join("yaml");
        write(&yaml_dir, "rust.yaml", RUST_RULES);
        write(&yaml_dir, "python.yml", MAPPING_RULES);

        let report = import_styleguide_yaml(&local, &yaml_dir).unwrap();
        assert_eq!(report.written, 3);
        assert!(report.skipped_existing.is_empty());
        assert!(report.findings.is_empty(), "{:?}", report.findings);

        // Locked path: language + category + rule id as the filename.
        let full = local
            .root()
            .join("styleguide/rust/error-handling/no-unwrap-in-prod.md");
        assert!(full.is_file());
        let concept = Concept::from_file(&full).unwrap();
        assert_eq!(concept.concept_type(), Some("Styleguide Rule"));
        assert_eq!(concept.get_str("language"), Some("rust"));
        assert_eq!(concept.get_str("category"), Some("error-handling"));
        assert_eq!(concept.get_str("rule_id"), Some("no-unwrap-in-prod"));
        assert_eq!(concept.get_str("priority"), Some("error"));
        assert_eq!(concept.get_str("pattern"), Some(".unwrap()"));
        let body = concept.body();
        assert!(body.contains("## Good"));
        assert!(body.contains("- let value = maybe()?;"));
        assert!(body.contains("## Bad"));
        assert!(body.contains("let value = maybe.unwrap();"));

        // Missing facets fall back to general/misc; single-string examples
        // stay unbulleted.
        let minimal = local.root().join("styleguide/general/misc/minimal-rule.md");
        let concept = Concept::from_file(&minimal).unwrap();
        assert_eq!(
            concept.get_str("description"),
            Some("A rule with only the required fields.")
        );
        assert!(!concept.body().contains("## Good"));

        let eval = local.root().join("styleguide/python/misc/no-eval.md");
        assert!(eval.is_file());
        let concept = Concept::from_file(&eval).unwrap();
        assert!(concept.body().contains("- eval(text)"));

        // The import produced STG-conformant rules end to end.
        assert_eq!(error_findings(&local).len(), 0);
    }

    #[test]
    fn import_is_additive_and_rerunnable() {
        let dir = TempDir::new().unwrap();
        let local = import_fixture(&dir);
        let yaml_dir = dir.path().join("yaml");
        write(&yaml_dir, "rust.yaml", RUST_RULES);

        import_styleguide_yaml(&local, &yaml_dir).unwrap();
        let second = import_styleguide_yaml(&local, &yaml_dir).unwrap();
        assert_eq!(second.written, 0);
        assert_eq!(
            second.skipped_existing,
            vec!["no-unwrap-in-prod".to_string(), "minimal-rule".to_string()]
        );
    }

    #[test]
    fn import_collects_bad_rules_as_findings_without_aborting() {
        let dir = TempDir::new().unwrap();
        let local = import_fixture(&dir);
        let yaml_dir = dir.path().join("yaml");
        write(&yaml_dir, "broken.yaml", BROKEN_RULES);
        write(&yaml_dir, "python.yml", MAPPING_RULES);

        let report = import_styleguide_yaml(&local, &yaml_dir).unwrap();
        assert_eq!(report.written, 1, "good rules still land");
        assert_eq!(report.findings.len(), 4);
        let messages: Vec<&str> = report.findings.iter().map(|f| f.message.as_str()).collect();
        assert!(
            messages
                .iter()
                .any(|m| m.contains("no-description-here") && m.contains("description")),
            "{messages:?}"
        );
        assert!(
            messages.iter().any(|m| m.contains("bad:colon")),
            "{messages:?}"
        );
        assert!(
            messages
                .iter()
                .any(|m| m.contains("bad-priority") && m.contains("error/warn/info")),
            "{messages:?}"
        );
        assert!(
            messages
                .iter()
                .any(|m| m.contains("numeric-priority") && m.contains("must be a string")),
            "{messages:?}"
        );
        assert!(error_findings(&local).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn import_records_unreadable_files_as_findings() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let local = import_fixture(&dir);
        let yaml_dir = dir.path().join("yaml");
        write(&yaml_dir, "locked.yaml", RUST_RULES);
        fs::set_permissions(
            yaml_dir.join("locked.yaml"),
            fs::Permissions::from_mode(0o000),
        )
        .unwrap();
        if fs::read_to_string(yaml_dir.join("locked.yaml")).is_ok() {
            // Running as root: permission bits don't gate reads, so the
            // unreadable-file path cannot be exercised in this environment.
            return;
        }
        write(&yaml_dir, "python.yml", MAPPING_RULES);

        let report = import_styleguide_yaml(&local, &yaml_dir).unwrap();
        assert_eq!(report.written, 1, "the batch must not abort");
        assert_eq!(report.findings.len(), 1);
        assert!(
            report.findings[0].message.contains("failed to read"),
            "{}",
            report.findings[0].message
        );
    }
}
