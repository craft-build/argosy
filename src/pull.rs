//! Pulling external argosies into a project or the global store.
//!
//! Argosy data lives under the user's state dir ([`state_dir`]), never in
//! the project tree: agents working the tree would just read the markdown
//! directly, bypassing the argosy MCP tools.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use snafu::{IntoError, OptionExt, ResultExt, ensure};

use crate::bundle::Argosy;
use crate::error::{IoSnafu, NotAnArgosySnafu, Result, ValidationSnafu};

/// The checkout name of a project's local (writable) bundle: `<state>/projects/<slug>/default`.
pub const LOCAL_ARGOSY_NAME: &str = "default";

/// The derived index file inside a project's state directory: `<state>/projects/<slug>/index.db`.
pub const INDEX_DB_NAME: &str = "index.db";

/// The sidecar in a project's state-dir slot recording its canonical project
/// root, so the catalog can map slots back to projects and flag stale ones.
pub const SLOT_ROOT_FILE: &str = ".project-root";

/// The sibling stamp `argosy pull` records beside every checkout: the SHA-256
/// of the sidecar it verified, or `absent` when the book shipped none.
///
/// It lives outside the cloned tree, so tampering with the checkout — the
/// sidecar rewritten or deleted included — is caught at the next open.
pub const IMPORT_INTEGRITY_SUFFIX: &str = ".import-integrity";

/// The stamp body for a checkout that shipped no integrity sidecar.
const STAMP_NO_SIDECAR: &str = "absent";

/// The user's argosy state root that every argosy path derives from:
/// `$XDG_STATE_HOME/argosy`, `~/.local/state/argosy`, or on Windows
/// `%USERPROFILE%\AppData\Local\argosy`.
pub fn state_dir() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| {
            std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(|home| home_state_base(PathBuf::from(home)))
        })
        .with_context(|| ValidationSnafu {
            reason: "cannot locate the argosy state directory: set XDG_STATE_HOME or HOME"
                .to_string(),
        })?;
    Ok(base.join("argosy"))
}

/// The per-user state base under `home`: `~/.local/state` on Unix,
/// `<home>\AppData\Local` on Windows.
fn home_state_base(home: PathBuf) -> PathBuf {
    if cfg!(windows) {
        home.join("AppData").join("Local")
    } else {
        home.join(".local").join("state")
    }
}

/// The directory of user-installed argosies shared by every project: `<state>/global`.
pub fn global_argosy_dir() -> Result<PathBuf> {
    Ok(state_dir()?.join("global"))
}

/// The state-dir slug of a project root: its final directory component plus
/// the first 8 hex digits of the SHA-256 of its absolute path, so
/// same-named projects never share storage.
///
/// The root is canonicalized first, so different spellings map to one slug.
pub fn project_slug(project_root: impl AsRef<Path>) -> String {
    let root = project_root.as_ref();
    let canonical = root
        .canonicalize()
        .or_else(|_| std::path::absolute(root))
        .unwrap_or_else(|_| root.to_path_buf());
    let name = canonical
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "project".to_string());
    let digest = crate::hash::sha256_hex(canonical.as_os_str().as_encoded_bytes());
    // The digest is lowercase ASCII hex, so slicing the first 8 bytes is safe.
    format!("{name}-{}", &digest[..8])
}

/// The directory holding a project's argosy state (local `default` bundle,
/// pulled checkouts, derived `index.db`), never in the project tree.
pub fn project_argosy_dir(project_root: impl AsRef<Path>) -> Result<PathBuf> {
    Ok(project_argosy_dir_at(&state_dir()?, project_root))
}

/// [`project_argosy_dir`] against an explicit state root (tests and hosts
/// inject a tempdir).
pub fn project_argosy_dir_at(state_root: &Path, project_root: impl AsRef<Path>) -> PathBuf {
    state_root.join("projects").join(project_slug(project_root))
}

/// The canonical spelling of `project_root`: its resolved path when the
/// directory exists, else its absolutized spelling.
///
/// The fallback keeps a deleted project's slot stable.
pub fn canonical_project_root(project_root: impl AsRef<Path>) -> PathBuf {
    let root = project_root.as_ref();
    root.canonicalize()
        .or_else(|_| std::path::absolute(root))
        .unwrap_or_else(|_| root.to_path_buf())
}

/// Records `project_root`'s canonical path in its state-dir slot, creating
/// the slot directory if needed, and returns the slot path.
pub fn record_project_root_at(
    state_root: &Path,
    project_root: impl AsRef<Path>,
) -> Result<PathBuf> {
    let slot = project_argosy_dir_at(state_root, project_root.as_ref());
    fs::create_dir_all(&slot).context(IoSnafu { path: slot.clone() })?;
    let sidecar = slot.join(SLOT_ROOT_FILE);
    let canonical = canonical_project_root(project_root);
    fs::write(&sidecar, canonical.to_string_lossy().as_bytes())
        .context(IoSnafu { path: sidecar })?;
    Ok(slot)
}

/// [`record_project_root_at`] against the resolved [`state_dir`].
pub fn record_project_root(project_root: impl AsRef<Path>) -> Result<PathBuf> {
    record_project_root_at(&state_dir()?, project_root)
}

/// The project root recorded for `slot` by [`record_project_root_at`], if any.
pub fn recorded_project_root(slot: &Path) -> Option<PathBuf> {
    let text = fs::read_to_string(slot.join(SLOT_ROOT_FILE)).ok()?;
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
}

/// Maps a failed `git` process spawn. A missing binary is a setup problem, not
/// an I/O problem on the destination.
fn git_spawn_error(dest: &Path, err: std::io::Error) -> crate::error::Error {
    if err.kind() == std::io::ErrorKind::NotFound {
        return ValidationSnafu {
            reason: "`git` was not found on PATH; `argosy pull` shells out to \
                     `git clone`, so install git (or fix PATH) and retry"
                .to_string(),
        }
        .build();
    }
    IoSnafu {
        path: dest.to_path_buf(),
    }
    .into_error(err)
}

/// Clones the argosy at `url` into `<root>/<name>` via `git clone`, opens it,
/// and verifies its integrity sidecar ([`crate::package::INTEGRITY_FILENAME`])
/// before accepting the checkout.
///
/// All-or-nothing: a failed, non-bundle, or integrity-failing clone leaves no
/// checkout behind.
pub fn clone_as_checkout(url: &str, root: &Path, name: &str) -> Result<Argosy> {
    // The URL reaches `git` verbatim, and this is a public API: option-shaped
    // URLs (`--upload-pack=<cmd>`, `ext::` transports) execute commands.
    ensure!(
        !url.is_empty() && !url.starts_with('-'),
        ValidationSnafu {
            reason: format!(
                "invalid clone url `{url}`: pass a git URL or remote name, not an option"
            )
        }
    );
    ensure!(
        crate::bundle::is_safe_bundle_name(name),
        ValidationSnafu {
            reason: format!(
                "invalid checkout name `{name}`: only [A-Za-z0-9._-] are allowed (it becomes the `argosy://` URI spelling)"
            )
        }
    );
    ensure!(
        name != LOCAL_ARGOSY_NAME,
        ValidationSnafu {
            reason: format!(
                "checkout name `{LOCAL_ARGOSY_NAME}` is reserved for the project-local bundle (create it with `argosy init`, never `argosy pull`)"
            )
        }
    );
    let dest = root.join(name);
    ensure!(
        !dest.exists(),
        ValidationSnafu {
            reason: format!(
                "checkout `{}` already exists; refusing to overwrite it",
                dest.display()
            )
        }
    );
    fs::create_dir_all(root).context(IoSnafu {
        path: root.to_path_buf(),
    })?;
    let output = Command::new("git")
        .args(["clone", "--quiet", url])
        .arg(&dest)
        .output()
        .map_err(|err| git_spawn_error(&dest, err))?;
    if !output.status.success() {
        let _ = fs::remove_dir_all(&dest);
        let stderr = String::from_utf8_lossy(&output.stderr);
        return ValidationSnafu {
            reason: format!(
                "`git clone {url}` failed ({status}): {stderr}",
                status = output.status,
                stderr = stderr.trim()
            ),
        }
        .fail();
    }
    match Argosy::open(&dest) {
        Ok(argosy) => {
            // A missing sidecar is fine: never packaged, nothing to verify.
            if sidecar_is_regular_file(&dest)
                && let Err(error) = crate::package::validate_integrity(&dest)
            {
                let _ = fs::remove_dir_all(&dest);
                return Err(error);
            }
            // The stamp is what makes a *later* tamper with the cloned tree
            // detectable.
            if let Err(error) = write_import_integrity_stamp(&dest) {
                let _ = fs::remove_dir_all(&dest);
                return Err(error);
            }
            Ok(argosy)
        }
        Err(error) => {
            let _ = fs::remove_dir_all(&dest);
            NotAnArgosySnafu {
                path: dest,
                reason: format!("`{url}` cloned, but the clone is not an argosy: {error:#}"),
            }
            .fail()
        }
    }
}

/// Whether an accepted checkout carries a verified sidecar: `"verified"` or
/// `"no sidecar"`.
pub fn integrity_status(checkout: &Path) -> &'static str {
    if sidecar_is_regular_file(checkout) {
        "verified"
    } else {
        "no sidecar"
    }
}

/// The pull-time stamp path beside a checkout: `<parent>/<name>.import-integrity`.
pub fn import_integrity_stamp(checkout: &Path) -> PathBuf {
    let name = checkout
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "checkout".to_string());
    checkout.with_file_name(format!("{name}{IMPORT_INTEGRITY_SUFFIX}"))
}

/// Whether the integrity sidecar is present as a regular file.
///
/// Symlinks count as absent: a symlinked sidecar is in attacker-reachable
/// space and must never be followed.
fn sidecar_is_regular_file(checkout: &Path) -> bool {
    std::fs::symlink_metadata(checkout.join(crate::package::INTEGRITY_FILENAME))
        .is_ok_and(|m| m.file_type().is_file())
}

/// The stamp body for a freshly pulled `checkout`: the SHA-256 of its
/// verified sidecar, or [`STAMP_NO_SIDECAR`] when it shipped none.
fn stamp_body(checkout: &Path) -> Result<String> {
    if !sidecar_is_regular_file(checkout) {
        return Ok(STAMP_NO_SIDECAR.to_string());
    }
    let path = checkout.join(crate::package::INTEGRITY_FILENAME);
    let bytes = fs::read(&path).context(IoSnafu { path })?;
    Ok(crate::hash::sha256_hex(&bytes))
}

/// Writes the pull-time integrity stamp beside `checkout`. Called only
/// after the checkout was accepted.
fn write_import_integrity_stamp(checkout: &Path) -> Result<()> {
    let stamp = import_integrity_stamp(checkout);
    fs::write(&stamp, stamp_body(checkout)?).context(IoSnafu { path: stamp })
}

/// Verifies a checkout against its pull-time stamp ([`IMPORT_INTEGRITY_SUFFIX`]);
/// `Err` means the checkout must be excluded from the active set — never served.
///
/// Unstamped checkouts follow the standing rule: a present sidecar is verified,
/// an absent one means nothing was ever packaged.
pub fn verify_import_integrity(checkout: &Path) -> std::result::Result<(), String> {
    let repull = "remove the checkout and `argosy pull` it again";
    let stamp = fs::read_to_string(import_integrity_stamp(checkout))
        .ok()
        .map(|s| s.trim().to_string());
    let sidecar_path = checkout.join(crate::package::INTEGRITY_FILENAME);
    match (stamp.as_deref(), sidecar_is_regular_file(checkout)) {
        (None, false) => Ok(()),
        (None, true) => crate::package::validate_integrity(checkout)
            .map(|_| ())
            .map_err(|e| format!("{e:#}")),
        (Some(STAMP_NO_SIDECAR), false) => Ok(()),
        (Some(STAMP_NO_SIDECAR), true) => Err(format!(
            "an integrity sidecar appeared after `argosy pull` (the book was pulled without \
             one); {repull}"
        )),
        (Some(expected), true) => {
            let bytes = fs::read(&sidecar_path)
                .map_err(|e| format!("cannot read the integrity sidecar: {e}"))?;
            if crate::hash::sha256_hex(&bytes) != expected {
                return Err(format!(
                    "the integrity sidecar changed since `argosy pull` (rewritten or \
                     re-committed upstream); {repull}"
                ));
            }
            crate::package::validate_integrity(checkout)
                .map(|_| ())
                .map_err(|e| format!("{e:#}"))
        }
        (Some(_), false) => Err(format!(
            "the integrity sidecar is missing — it was verified when the book was pulled, so \
             the checkout changed underneath it; {repull}"
        )),
    }
}

/// The commit a checkout's git metadata resolves to, if any — the provenance
/// hits and reads report for imported content, and `None` is not an error.
///
/// Read from git files, not a subprocess, so opening imports stays cheap.
pub fn checkout_head_commit(checkout: &Path) -> Option<String> {
    fn is_commit_id(text: &str) -> bool {
        let text = text.trim();
        (text.len() == 40 || text.len() == 64) && text.bytes().all(|b| b.is_ascii_hexdigit())
    }

    let git = checkout.join(".git");
    let metadata = fs::symlink_metadata(&git).ok()?;
    // A `.git` file points at the real git dir (worktrees, submodules);
    // `Path::join` keeps an absolute target as-is.
    let git_dir = if metadata.file_type().is_dir() {
        git
    } else {
        let pointer = fs::read_to_string(&git).ok()?;
        let target = pointer.strip_prefix("gitdir:")?.trim();
        checkout.join(target)
    };
    let head = fs::read_to_string(git_dir.join("HEAD"))
        .ok()?
        .trim()
        .to_string();
    // Detached HEAD names the commit directly.
    if is_commit_id(&head) {
        return Some(head);
    }
    let ref_name = head.strip_prefix("ref: ")?.trim();
    if let Ok(loose) = fs::read_to_string(git_dir.join(ref_name))
        && is_commit_id(&loose)
    {
        return Some(loose.trim().to_string());
    }
    // `^`-prefixed peel lines fail the shape check and drop out.
    let packed = fs::read_to_string(git_dir.join("packed-refs")).ok()?;
    packed.lines().find_map(|line| {
        let (hash, name) = line.split_once(' ')?;
        (name.trim() == ref_name && is_commit_id(hash)).then(|| hash.to_string())
    })
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use tempfile::TempDir;

    use super::*;

    /// Builds a minimal bundle at `dir` and commits it as a git repo.
    fn git_repo(dir: &Path) {
        fs::create_dir_all(dir).unwrap();
        fs::write(
            dir.join("argosy.md"),
            "---\ntype: Argosy Manifest\nname: remote-rules\nargosy_version: \"1.2.0\"\n---\n# remote-rules\n",
        )
        .unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(dir)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?} failed");
        };
        git(&["init", "--quiet"]);
        git(&["-c", "user.name=t", "-c", "user.email=t@t", "add", "-A"]);
        git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "--quiet",
            "-m",
            "x",
        ]);
    }

    #[test]
    fn git_spawn_error_names_git_when_the_binary_is_missing() {
        // Regression: a failed spawn used to surface as a misleading I/O error
        // on the destination path.
        let err = git_spawn_error(
            Path::new("/tmp/dest"),
            std::io::Error::from(std::io::ErrorKind::NotFound),
        );
        let msg = err.to_string();
        assert!(msg.contains("`git` was not found on PATH"), "got {msg}");
        assert!(!msg.contains("dest"), "no misleading path: {msg}");

        // Any other spawn failure stays an I/O error on the destination.
        let err = git_spawn_error(
            Path::new("/tmp/dest"),
            std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        );
        assert!(matches!(err, crate::error::Error::Io { .. }), "got {err:?}");
    }

    /// `--upload-pack=<cmd>` and `ext::` transports execute commands.
    #[test]
    fn clone_as_checkout_refuses_option_like_urls() {
        for bad in ["--upload-pack=touch /tmp/pwned", "-", ""] {
            let err = clone_as_checkout(bad, Path::new("/tmp/none"), "name").unwrap_err();
            assert!(
                err.to_string().contains("invalid clone url"),
                "{bad:?}: {err}"
            );
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn home_state_base_is_posix_local_state() {
        assert_eq!(
            home_state_base(PathBuf::from("/home/u")),
            PathBuf::from("/home/u/.local/state")
        );
    }

    #[test]
    fn project_slug_is_name_plus_a_short_hash_of_the_absolute_path() {
        let a = Path::new("/home/me/Projects/craft");
        let slug = project_slug(a);
        assert!(
            slug.starts_with("craft-") && slug.len() == "craft-".len() + 8,
            "got {slug}"
        );
        assert!(
            slug["craft-".len()..]
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "hash is lowercase hex: {slug}"
        );
        assert_ne!(slug, project_slug(Path::new("/home/you/Projects/craft")));
        assert_eq!(slug, project_slug(a));
    }

    #[cfg(unix)]
    #[test]
    fn project_slug_canonicalizes_so_symlinked_spellings_share_one_slug() {
        let tmp = TempDir::new().unwrap();
        let real = tmp.path().join("real-project");
        fs::create_dir_all(&real).unwrap();
        let link = tmp.path().join("linked");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(project_slug(&real), project_slug(&link));
    }

    #[test]
    fn project_argosy_dir_at_nests_the_slug_under_projects() {
        let dir = project_argosy_dir_at(Path::new("/state/argosy"), "/home/me/Projects/craft");
        assert_eq!(
            dir.parent().unwrap(),
            Path::new("/state/argosy/projects"),
            "got {}",
            dir.display()
        );
        assert!(
            dir.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("craft-")),
            "got {}",
            dir.display()
        );
    }

    #[test]
    fn record_project_root_writes_and_reads_back_the_canonical_root() {
        let tmp = TempDir::new().unwrap();
        let state = tmp.path().join("state/argosy");
        let project = tmp.path().join("work/myproj");
        fs::create_dir_all(&project).unwrap();

        let slot = record_project_root_at(&state, &project).unwrap();
        assert_eq!(slot, project_argosy_dir_at(&state, &project));
        let recorded = recorded_project_root(&slot).unwrap();
        assert_eq!(recorded, canonical_project_root(&project));
        assert!(recorded.is_absolute());

        // A deleted project keeps its absolutized spelling so stale slots stay detectable.
        fs::remove_dir_all(&project).unwrap();
        assert_eq!(recorded_project_root(&slot), Some(recorded));
    }

    #[test]
    fn recorded_project_root_is_none_without_a_sidecar() {
        let tmp = TempDir::new().unwrap();
        assert_eq!(recorded_project_root(tmp.path()), None);
    }

    #[test]
    fn clone_pulls_a_bundle_into_a_named_checkout() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        git_repo(&repo);
        let root = tmp.path().join("state/projects/craft-1a2b3c4d");

        let argosy = clone_as_checkout(repo.to_str().unwrap(), &root, "company-rules").unwrap();

        assert_eq!(argosy.manifest().name(), "remote-rules");
        assert_eq!(argosy.manifest().argosy_version().to_string(), "1.2.0");
        assert!(root.join("company-rules/argosy.md").is_file());
    }

    #[test]
    fn clone_refuses_an_existing_checkout_and_bad_names() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("store");
        fs::create_dir_all(root.join("taken")).unwrap();

        let err = clone_as_checkout("irrelevant", &root, "taken").unwrap_err();
        assert!(err.to_string().contains("refusing to overwrite"), "{err}");

        let err = clone_as_checkout("irrelevant", &root, "bad name").unwrap_err();
        assert!(err.to_string().contains("invalid checkout name"), "{err}");

        let err = clone_as_checkout("irrelevant", &root, LOCAL_ARGOSY_NAME).unwrap_err();
        assert!(err.to_string().contains("reserved"), "{err}");
        assert!(!root.join(LOCAL_ARGOSY_NAME).exists(), "nothing created");
    }

    #[test]
    fn clone_of_a_non_argosy_repo_leaves_no_checkout() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("plain-repo");
        fs::create_dir_all(&repo).unwrap();
        for (args, pwd) in [
            (vec!["init", "--quiet"], repo.clone()),
            (
                vec![
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "commit",
                    "--quiet",
                    "--allow-empty",
                    "-m",
                    "x",
                ],
                repo.clone(),
            ),
        ] {
            let out = Command::new("git")
                .args(&args)
                .current_dir(pwd)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?} failed");
        }

        let root = tmp.path().join("store");
        let err = clone_as_checkout(repo.to_str().unwrap(), &root, "notargosy").unwrap_err();

        assert!(err.to_string().contains("not an argosy"), "{err}");
        assert!(!root.join("notargosy").exists(), "partial checkout removed");
    }

    /// Builds a repo from a *packaged* bundle, so clones carry a sidecar.
    fn packaged_repo(dir: &Path) {
        fs::create_dir_all(dir).unwrap();
        let source = tempfile::tempdir().unwrap();
        fs::write(
            source.path().join("argosy.md"),
            "---\ntype: Argosy Manifest\nname: remote-rules\nargosy_version: \"1.2.0\"\n---\n# remote-rules\n",
        )
        .unwrap();
        fs::create_dir_all(source.path().join("document")).unwrap();
        fs::write(source.path().join("document/guide.md"), "Guide.\n").unwrap();
        crate::package::package(
            &crate::bundle::Argosy::open(source.path()).unwrap(),
            dir,
            &crate::package::PackageOptions::default(),
        )
        .unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(dir)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?} failed");
        };
        git(&["init", "--quiet"]);
        git(&["-c", "user.name=t", "-c", "user.email=t@t", "add", "-A"]);
        git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "--quiet",
            "-m",
            "x",
        ]);
    }

    #[test]
    fn clone_accepts_a_repo_whose_sidecar_matches_its_contents() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("pkg-repo");
        packaged_repo(&repo);
        let root = tmp.path().join("store");

        let argosy = clone_as_checkout(repo.to_str().unwrap(), &root, "shared").unwrap();

        assert_eq!(argosy.manifest().name(), "remote-rules");
        assert_eq!(integrity_status(&root.join("shared")), "verified");
    }

    #[test]
    fn pull_records_an_integrity_stamp_beside_the_checkout() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("pkg-repo");
        packaged_repo(&repo);
        let root = tmp.path().join("store");

        clone_as_checkout(repo.to_str().unwrap(), &root, "shared").unwrap();

        // The stamp is the importer's record of the verified sidecar, outside the clone.
        let stamp = fs::read_to_string(import_integrity_stamp(&root.join("shared"))).unwrap();
        let sidecar =
            fs::read(root.join("shared").join(crate::package::INTEGRITY_FILENAME)).unwrap();
        assert_eq!(stamp, crate::hash::sha256_hex(&sidecar));
        assert_eq!(stamp.len(), 64, "a sha256 hex digest: {stamp}");

        assert!(verify_import_integrity(&root.join("shared")).is_ok());
    }

    #[test]
    fn a_sidecarless_pull_stamps_absent() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        git_repo(&repo); // hand-rolled: no sidecar
        let root = tmp.path().join("store");

        clone_as_checkout(repo.to_str().unwrap(), &root, "shared").unwrap();

        let stamp = fs::read_to_string(import_integrity_stamp(&root.join("shared"))).unwrap();
        assert_eq!(stamp, "absent");
        assert!(verify_import_integrity(&root.join("shared")).is_ok());
    }

    #[test]
    fn clone_refuses_a_tampered_sidecar_and_leaves_no_checkout() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("pkg-repo");
        packaged_repo(&repo);
        // Tamper after the commit — this is what a rug-pulled repo looks like.
        fs::write(repo.join("document/guide.md"), "Injected.\n").unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(&repo)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?} failed");
        };
        git(&["-c", "user.name=t", "-c", "user.email=t@t", "add", "-A"]);
        git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "--quiet",
            "-m",
            "tamper",
        ]);
        let root = tmp.path().join("store");

        let err = clone_as_checkout(repo.to_str().unwrap(), &root, "shared").unwrap_err();

        assert!(
            err.to_string().contains("integrity"),
            "error names the integrity failure: {err}"
        );
        assert!(
            !root.join("shared").exists(),
            "refused clone leaves nothing"
        );
    }

    #[test]
    fn a_clone_without_a_sidecar_is_accepted_with_nothing_to_verify() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        git_repo(&repo); // hand-rolled: no sidecar
        let root = tmp.path().join("store");

        let argosy = clone_as_checkout(repo.to_str().unwrap(), &root, "shared").unwrap();

        assert_eq!(argosy.manifest().name(), "remote-rules");
        assert_eq!(integrity_status(&root.join("shared")), "no sidecar");
    }

    #[test]
    fn a_clone_reports_its_head_commit() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        git_repo(&repo);
        let root = tmp.path().join("store");
        clone_as_checkout(repo.to_str().unwrap(), &root, "shared").unwrap();

        let commit = checkout_head_commit(&root.join("shared")).unwrap();
        assert_eq!(commit.len(), 40, "a sha1 object id: {commit}");
        assert!(
            commit.bytes().all(|b| b.is_ascii_hexdigit()),
            "lowercase hex: {commit}"
        );
        // It is genuinely the remote's HEAD, not a ref spelling.
        let out = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&repo)
            .output()
            .unwrap();
        assert_eq!(commit, String::from_utf8_lossy(&out.stdout).trim());
    }

    #[test]
    fn head_commit_resolves_a_packed_ref() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        git_repo(&repo);
        let root = tmp.path().join("store");
        clone_as_checkout(repo.to_str().unwrap(), &root, "shared").unwrap();

        // After a server-side repack the branch lives only in packed-refs.
        let git_dir = root.join("shared/.git");
        let head = fs::read_to_string(git_dir.join("HEAD")).unwrap();
        let ref_name = head.trim().strip_prefix("ref: ").unwrap().to_string();
        let loose = fs::read_to_string(git_dir.join(&ref_name))
            .unwrap()
            .trim()
            .to_string();
        fs::remove_file(git_dir.join(&ref_name)).unwrap();
        fs::write(
            git_dir.join("packed-refs"),
            format!("# pack-refs-with: peeled fully-peeled sorted\n{loose} {ref_name}\n"),
        )
        .unwrap();

        assert_eq!(
            checkout_head_commit(&root.join("shared")).unwrap(),
            loose,
            "packed-refs answers when the loose ref is gone"
        );
    }

    #[test]
    fn a_detached_head_reports_the_commit_directly() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        git_repo(&repo);
        let root = tmp.path().join("store");
        clone_as_checkout(repo.to_str().unwrap(), &root, "shared").unwrap();
        let out = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(root.join("shared"))
            .output()
            .unwrap();
        let commit = String::from_utf8_lossy(&out.stdout).trim().to_string();
        fs::write(root.join("shared/.git/HEAD"), &commit).unwrap();

        assert_eq!(checkout_head_commit(&root.join("shared")).unwrap(), commit);
    }

    #[test]
    fn a_checkout_without_git_metadata_reports_no_commit() {
        let tmp = TempDir::new().unwrap();
        // A plain tree (hosts point imports at these) and garbage HEADs.
        fs::create_dir_all(tmp.path().join("plain/argosy.md").parent().unwrap()).unwrap();
        assert_eq!(checkout_head_commit(&tmp.path().join("plain")), None);

        let junk = tmp.path().join("junk");
        fs::create_dir_all(junk.join(".git")).unwrap();
        fs::write(junk.join(".git/HEAD"), "not a ref\n").unwrap();
        assert_eq!(checkout_head_commit(&junk), None);
    }
}
