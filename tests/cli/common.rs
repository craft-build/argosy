//! Shared helpers: the compiled binary, fixture copies, and scratch XDG state homes.

use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use tempfile::TempDir;

use argosy::pull;

pub(crate) fn argosy_bin() -> Command {
    Command::cargo_bin("argosy").expect("binary builds with the package")
}

pub(crate) fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Small recursive copy (fixtures are tiny).
pub(crate) fn copy_dir(source: &Path, dest: &Path) {
    fs::create_dir_all(dest).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = dest.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// Copies a fixture into a temp directory without the `.argosy/` cache, so
/// tests start from a clean slate and never touch the checked-in fixtures.
pub(crate) fn fixture_copy(name: &str, scratch: &TempDir) -> PathBuf {
    let dest = scratch.path().join(name);
    copy_dir(&fixture(name), &dest);
    let cache = dest.join(".argosy");
    if cache.exists() {
        fs::remove_dir_all(&cache).unwrap();
    }
    dest
}

/// A fresh XDG state home for `XDG_STATE_HOME`, isolated from the user's `~/.local/state`.
pub(crate) fn xdg_state_home(scratch: &TempDir) -> PathBuf {
    let home = scratch.path().join("xdg-state-home");
    fs::create_dir_all(&home).unwrap();
    home
}

/// A standard-layout project: the `default` bundle (a valid-fixture copy)
/// lives under the state dir; returns `(project cwd, XDG_STATE_HOME to inject)`.
#[cfg(feature = "default-index")] // only the gated index tests build projects
pub(crate) fn fixture_project(scratch: &TempDir) -> (PathBuf, PathBuf) {
    let project = scratch.path().join("project");
    fs::create_dir_all(&project).unwrap();
    let xdg = xdg_state_home(scratch);
    let local = pull::project_argosy_dir_at(&xdg.join("argosy"), &project).join("default");
    copy_dir(&fixture("valid-acme-billing"), &local);
    let cache = local.join(".argosy");
    if cache.exists() {
        fs::remove_dir_all(&cache).unwrap();
    }
    (project, xdg)
}

/// Initializes `dir` as a git repo with one commit; local clones need no network.
pub(crate) fn git_commit_all(dir: &Path) {
    for args in [
        vec!["init", "--quiet"],
        vec!["-c", "user.name=t", "-c", "user.email=t@t", "add", "-A"],
        vec![
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "--quiet",
            "-m",
            "x",
        ],
    ] {
        let out = std::process::Command::new("git")
            .args(&args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?} failed");
    }
}
