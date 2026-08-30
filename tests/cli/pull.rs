//! `pull` tests.

use std::fs;

use predicates::prelude::*;

use tempfile::TempDir;

use argosy::pull;

use super::common::*;

#[test]
fn pull_clones_a_remote_bundle_into_the_project() {
    let scratch = TempDir::new().unwrap();
    let repo = fixture_copy("valid-acme-billing", &scratch);
    git_commit_all(&repo);
    let project = scratch.path().join("project");
    fs::create_dir_all(&project).unwrap();
    let xdg = xdg_state_home(&scratch);
    let store = pull::project_argosy_dir_at(&xdg.join("argosy"), &project);

    argosy_bin()
        .args(["pull", repo.to_str().unwrap(), "company-rules"])
        .current_dir(&project)
        .env("XDG_STATE_HOME", &xdg)
        .assert()
        .success()
        .stdout(predicate::str::contains("pulled acme-billing 0.3.1 into"))
        .stdout(predicate::str::contains("company-rules"));
    assert!(store.join("company-rules/argosy.md").is_file());
    assert!(!project.join(".argosy").exists(), "tree stays argosy-free");

    // A checkout is never overwritten.
    argosy_bin()
        .args(["pull", repo.to_str().unwrap(), "company-rules"])
        .current_dir(&project)
        .env("XDG_STATE_HOME", &xdg)
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("refusing to overwrite"));
}

#[test]
fn pull_of_a_non_argosy_repo_leaves_no_checkout() {
    let scratch = TempDir::new().unwrap();
    let repo = scratch.path().join("plain-repo");
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("README.md"), "not a bundle").unwrap();
    git_commit_all(&repo);
    let project = scratch.path().join("project");
    fs::create_dir_all(&project).unwrap();
    let xdg = xdg_state_home(&scratch);
    let store = pull::project_argosy_dir_at(&xdg.join("argosy"), &project);

    argosy_bin()
        .args(["pull", repo.to_str().unwrap(), "notargosy"])
        .current_dir(&project)
        .env("XDG_STATE_HOME", &xdg)
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("not an argosy"));
    assert!(!store.join("notargosy").exists());
}

#[test]
fn pull_global_installs_into_the_user_store() {
    let scratch = TempDir::new().unwrap();
    let repo = fixture_copy("valid-acme-billing", &scratch);
    git_commit_all(&repo);
    let fake_home = scratch.path().join("home");

    argosy_bin()
        .args(["pull", "--global", repo.to_str().unwrap(), "shared-rules"])
        .current_dir(scratch.path())
        .env("HOME", &fake_home)
        .assert()
        .success();
    assert!(
        fake_home
            .join(".local/state/argosy/global/shared-rules/argosy.md")
            .is_file()
    );
}

// ------------------------------------------------------------------- index
// All index tests are gated: without the `default-index`
// feature the binary refuses the subcommand entirely.
