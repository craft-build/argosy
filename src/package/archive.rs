//! The packaging pipeline: directory and tar.gz materialization, the
//! content hash, and sidecar validation.

use std::fs;
use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};
use snafu::ResultExt;

use crate::bundle::Argosy;
use crate::error::{
    Error, IntegrityMismatchSnafu, IoSnafu, NotAnArgosySnafu, Result, ValidationSnafu,
};
use crate::hash::{hex, sha256_hex};

use super::payload::{
    INDEX_DIR, MEMORY_DIR, collect_payload, integrity_text, posix, read_bundle_file, staging_path,
    walk_copied_files,
};
use super::{INTEGRITY_FILENAME, PackageFormat, PackageOptions, PackageReport};

/// Copies `source` to `dest` for distribution. `dest` must be empty for
/// [`PackageFormat::Directory`] and lie outside the source bundle. Both
/// formats stage to a sibling temp path and rename on success, so a
/// mid-copy failure leaves no partial tree or truncated archive.
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

    // Probe before copying so the exclusion is *visible* whenever memory/
    // existed at packaging time, even though it always works.
    let memory_excluded = fs::symlink_metadata(root.join(MEMORY_DIR)).is_ok();
    #[cfg(feature = "default-index")]
    if options.include_index {
        crate::index::sqlite::checkpoint_wal(&root.join(INDEX_DIR).join("index.db"))?;
    }

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
            // Symmetry with Directory mode: never clobber an existing
            // artifact (which also makes the rename cross-platform —
            // Windows errors on rename-over-existing).
            if dest.exists() {
                return ValidationSnafu {
                    reason: format!(
                        "packaging destination `{}` already exists; refusing to overwrite it",
                        dest.display()
                    ),
                }
                .fail();
            }
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
                    // mtime 0 (the GNU default for unset) is a deliberate
                    // reproducibility choice: identical bundles produce
                    // byte-identical archives. Some strict extractors warn
                    // about epoch timestamps — that is expected.
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
/// everything [`package`] would copy — a change detector independent of
/// `argosy_version` bumps, stable across a packaged copy. Errors when
/// `argosy_root` is not a bundle root (no `argosy.md`).
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

/// Recomputes every hash in the bundle's integrity sidecar. A
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
        // (containment — this covers literal `..` components, while
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
    // The writer lists `.argosy/**` when it shipped them, so
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
