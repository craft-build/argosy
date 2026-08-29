//! Pulling external argosies into a project or the global store. A
//! project's argosies all live in `.argosy/`: the local bundle at
//! `.argosy/default`, pulled checkouts at `.argosy/<name>/`, the index at
//! `.argosy/index.db` — outside every bundle, so bundles stay clean to
//! `package` and plain `git`. Global checkouts: [`global_argosy_dir`].

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use snafu::{OptionExt, ResultExt, ensure};

use crate::bundle::Argosy;
use crate::error::{IoSnafu, NotAnArgosySnafu, Result, ValidationSnafu};

/// The project directory holding the local bundle, pulled checkouts, and
/// the derived index (`<project>/.argosy/`).
pub const PROJECT_ARGOSY_DIR: &str = ".argosy";

/// The checkout name of a project's local (writable) bundle:
/// `<project>/.argosy/default`.
pub const LOCAL_ARGOSY_NAME: &str = "default";

/// The directory holding argosies installed for the user, shared by every
/// project: `$XDG_STATE_HOME/argosy` (falling back to `~/.local/state/argosy`).
pub fn global_argosy_dir() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .with_context(|| ValidationSnafu {
            reason: "cannot locate the global argosy directory: set XDG_STATE_HOME or HOME"
                .to_string(),
        })?;
    Ok(base.join("argosy"))
}

/// Clones the argosy at `url` into `<root>/<name>` via `git clone` and
/// opens it, refusing to overwrite an existing checkout and rejecting
/// `name` outside the URI charset (see
/// [`crate::bundle::is_safe_bundle_name`]). All-or-nothing: a failed or
/// non-bundle clone leaves no partial checkout behind.
pub fn clone_as_checkout(url: &str, root: &Path, name: &str) -> Result<Argosy> {
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
        .context(IoSnafu { path: dest.clone() })?;
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
    fn clone_pulls_a_bundle_into_a_named_checkout() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        git_repo(&repo);
        let root = tmp.path().join("project/.argosy");

        let argosy = clone_as_checkout(repo.to_str().unwrap(), &root, "company-rules").unwrap();

        assert_eq!(argosy.manifest().name(), "remote-rules");
        assert_eq!(argosy.manifest().argosy_version().to_string(), "1.2.0");
        assert!(root.join("company-rules/argosy.md").is_file());
    }

    #[test]
    fn clone_refuses_an_existing_checkout_and_bad_names() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join(".argosy");
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

        let root = tmp.path().join(".argosy");
        let err = clone_as_checkout(repo.to_str().unwrap(), &root, "notargosy").unwrap_err();

        assert!(err.to_string().contains("not an argosy"), "{err}");
        assert!(!root.join("notargosy").exists(), "partial checkout removed");
    }
}
