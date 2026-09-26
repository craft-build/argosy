//! `catalog` tests: the read-only global catalog and its `--write` file.

use std::fs;
use std::path::Path;
use tempfile::TempDir;

use predicates::prelude::*;

use super::common::*;

/// Initializes `project` as the default project slot under `xdg`, recording
/// its canonical root. Returns the project directory.
fn init_project(scratch: &TempDir, xdg: &Path, name: &str) -> std::path::PathBuf {
    let project = scratch.path().join(name);
    fs::create_dir_all(&project).unwrap();
    argosy_bin()
        .args(["--json", "init"])
        .current_dir(&project)
        .env("XDG_STATE_HOME", xdg)
        .assert()
        .success();
    project
}

#[test]
fn catalog_prints_markdown_for_a_registered_project() {
    let scratch = TempDir::new().unwrap();
    let xdg = xdg_state_home(&scratch);
    init_project(&scratch, &xdg, "catproj");

    argosy_bin()
        .args(["catalog"])
        .env("XDG_STATE_HOME", &xdg)
        .assert()
        .success()
        .stdout(predicate::str::contains("# Argosy catalog"))
        .stdout(predicate::str::contains("Local bundle: catproj (writable)"))
        .stdout(predicate::str::contains("Project root:"))
        .stdout(predicate::str::contains("Project directory exists: yes"))
        .stdout(predicate::str::contains("- Index: absent"));

    // The slot records the canonical project root for stale detection.
    let slot =
        argosy::pull::project_argosy_dir_at(&xdg.join("argosy"), scratch.path().join("catproj"));
    assert!(argosy::pull::recorded_project_root(&slot).is_some());
}

#[test]
fn catalog_json_serializes_the_catalog() {
    let scratch = TempDir::new().unwrap();
    let xdg = xdg_state_home(&scratch);
    init_project(&scratch, &xdg, "jsonproj");

    let output = argosy_bin()
        .args(["catalog", "--json"])
        .env("XDG_STATE_HOME", &xdg)
        .assert()
        .success()
        .get_output()
        .clone();
    let catalog: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let projects = catalog["projects"].as_array().unwrap();
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0]["local"]["name"], "jsonproj");
    assert_eq!(projects[0]["local"]["writable"], true);
    assert_eq!(projects[0]["root_exists"], true);
    assert_eq!(projects[0]["stale"], false);
    assert_eq!(projects[0]["contents"]["documents"], 0);
    assert_eq!(projects[0]["index"]["present"], false);
}

#[test]
fn catalog_write_materializes_readme_at_the_state_root() {
    let scratch = TempDir::new().unwrap();
    let xdg = xdg_state_home(&scratch);
    init_project(&scratch, &xdg, "writeproj");

    argosy_bin()
        .args(["catalog", "--write"])
        .env("XDG_STATE_HOME", &xdg)
        .assert()
        .success()
        .stdout(predicate::str::contains("wrote catalog to"));

    let readme = xdg.join("argosy").join("README.md");
    assert!(readme.is_file(), "catalog written");
    let text = fs::read_to_string(&readme).unwrap();
    assert!(text.contains("# Argosy catalog"), "got {text}");
    assert!(text.contains("writeproj"), "got {text}");
}

#[test]
fn catalog_flags_a_stale_slot_after_the_project_directory_is_removed() {
    let scratch = TempDir::new().unwrap();
    let xdg = xdg_state_home(&scratch);
    let project = init_project(&scratch, &xdg, "staleproj");
    fs::remove_dir_all(&project).unwrap();

    argosy_bin()
        .args(["catalog"])
        .env("XDG_STATE_HOME", &xdg)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Project directory exists: no (stale slot)",
        ))
        .stdout(predicate::str::contains("Stale:"))
        .stdout(predicate::str::contains("staleproj"));
}

#[test]
fn catalog_redact_home_rewrites_the_project_root_prefix() {
    let scratch = TempDir::new().unwrap();
    let home = scratch.path().join("home");
    fs::create_dir_all(&home).unwrap();
    // Put state and project under a fake HOME so redaction has something to
    // strip; both XDG_STATE_HOME and HOME are set.
    let xdg = home.join(".local/state");
    fs::create_dir_all(&xdg).unwrap();
    init_project_under(&xdg, &home, "redactproj");

    argosy_bin()
        .args(["catalog", "--redact-home"])
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &xdg)
        .assert()
        .success()
        .stdout(predicate::str::contains("~/redactproj"))
        .stdout(predicate::str::contains("~/.local/state/argosy"));
}

/// Like [`init_project`] but the project lives under `home`, with `HOME`
/// injected alongside `XDG_STATE_HOME`.
fn init_project_under(xdg: &Path, home: &Path, name: &str) -> std::path::PathBuf {
    let project = home.join(name);
    fs::create_dir_all(&project).unwrap();
    argosy_bin()
        .args(["--json", "init"])
        .current_dir(&project)
        .env("HOME", home)
        .env("XDG_STATE_HOME", xdg)
        .assert()
        .success();
    project
}
