//! Pulling external argosies into a project or the global store. Argosy
//! data never lives in the project tree — agents working the tree would
//! just read the markdown directly, bypassing the argosy MCP tools — so
//! everything lives under the user's argosy state dir
//! ([`state_dir`]: `$XDG_STATE_HOME/argosy`, `~/.local/state/argosy`):
//!
//! - user-wide checkouts at `<state>/global/<name>/`,
//! - a project's checkouts at `<state>/projects/<slug>/<name>/`, with the
//!   local (writable) bundle at `<state>/projects/<slug>/default/` and the
//!   derived index at `<state>/projects/<slug>/index.db`.
//!
//! `<slug>` is [`project_slug`]: the project root's directory name plus a
//! short hash of its absolute path, so same-named projects never collide.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use snafu::{IntoError, OptionExt, ResultExt, ensure};

use crate::bundle::Argosy;
use crate::error::{IoSnafu, NotAnArgosySnafu, Result, ValidationSnafu};

/// The checkout name of a project's local (writable) bundle:
/// `<state>/projects/<slug>/default`.
pub const LOCAL_ARGOSY_NAME: &str = "default";

/// The derived index file inside a project's state directory:
/// `<state>/projects/<slug>/index.db`.
pub const INDEX_DB_NAME: &str = "index.db";

/// The user's argosy state root: `$XDG_STATE_HOME/argosy` (falling back
/// to `~/.local/state/argosy`, and on Windows — where neither
/// `$XDG_STATE_HOME` nor `$HOME` is set by default — to
/// `%USERPROFILE%\AppData\Local\argosy`). Every argosy path — global
/// checkouts, project checkouts, indexes — derives from here, keeping
/// argosy data out of the project tree entirely.
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
/// `<home>\AppData\Local` on Windows (which has no POSIX home layout).
fn home_state_base(home: PathBuf) -> PathBuf {
    if cfg!(windows) {
        home.join("AppData").join("Local")
    } else {
        home.join(".local").join("state")
    }
}

/// The directory holding argosies installed for the user, shared by every
/// project: `<state>/global`.
pub fn global_argosy_dir() -> Result<PathBuf> {
    Ok(state_dir()?.join("global"))
}

/// The state-dir slug of a project root: its final directory component
/// plus the first 8 hex digits of the SHA-256 of its absolute path (e.g.
/// `craft-1a2b3c4d`), so two same-named projects never share storage.
/// The root is canonicalized first, so different spellings of one
/// directory (symlinked or not) map to one slug.
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
    format!("{name}-{}", &digest[..8])
}

/// The directory holding a project's argosy state: the local (writable)
/// bundle at `<...>/default`, pulled checkouts at `<...>/<name>`, and the
/// derived index at `<...>/index.db` — under
/// `$XDG_STATE_HOME/argosy/projects/<slug>`, never in the project tree.
pub fn project_argosy_dir(project_root: impl AsRef<Path>) -> Result<PathBuf> {
    Ok(project_argosy_dir_at(&state_dir()?, project_root))
}

/// [`project_argosy_dir`] against an explicit state root (tests and hosts
/// inject a tempdir instead of touching `~/.local/state`).
pub fn project_argosy_dir_at(state_root: &Path, project_root: impl AsRef<Path>) -> PathBuf {
    state_root.join("projects").join(project_slug(project_root))
}

/// Maps a failed `git` process spawn. A missing binary is a setup problem,
/// not an I/O problem on the destination — name `git` and PATH so the user
/// can act on the message instead of chasing a misleading path.
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

/// Clones the argosy at `url` into `<root>/<name>` via `git clone` and
/// opens it, refusing to overwrite an existing checkout and rejecting
/// `name` outside the URI charset (see
/// [`crate::bundle::is_safe_bundle_name`]). All-or-nothing: a failed or
/// non-bundle clone leaves no partial checkout behind.
pub fn clone_as_checkout(url: &str, root: &Path, name: &str) -> Result<Argosy> {
    // The URL reaches `git` verbatim: a leading dash would be parsed as a
    // git option (`--upload-pack=<cmd>` executes commands), and `ext::`
    // transports execute local commands by design. The CLI caller is the
    // local user, but this is a public library API — refuse option-shaped
    // URLs up front rather than trusting every future caller.
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
        Ok(argosy) => Ok(argosy),
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
        // Regression: a failed `git` spawn used to surface as an I/O error
        // on the *destination path* — misleading on machines without git.
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

    /// Option-shaped URLs must be refused before they reach `git clone`:
    /// `--upload-pack=<cmd>` (or an `ext::` transport) executes commands.
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

    // --- state-dir layout ---

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
        // Same-named projects at different paths never collide…
        assert_ne!(slug, project_slug(Path::new("/home/you/Projects/craft")));
        // …and one path is stable across calls.
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
}
