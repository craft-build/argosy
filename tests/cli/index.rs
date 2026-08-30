//! `index` tests (plus the ignored real-backend round trip).

use std::fs;
use tempfile::TempDir;

use predicates::prelude::*;

use argosy::pull;

use super::common::*;

#[test]
fn index_status_reports_a_missing_index_without_creating_one() {
    let scratch = TempDir::new().unwrap();
    let (project, xdg) = fixture_project(&scratch);
    argosy_bin()
        .args(["index", "status"])
        .current_dir(&project)
        .env("XDG_STATE_HOME", &xdg)
        .assert()
        .success()
        .stdout(predicate::str::contains("no index at"));
    let store = pull::project_argosy_dir_at(&xdg.join("argosy"), &project);
    assert!(!store.join("index.db").exists(), "status never writes");
    assert!(!project.join(".argosy").exists(), "tree stays argosy-free");
}

#[cfg(feature = "default-index")]
#[test]
fn index_on_a_project_without_a_local_bundle_points_at_init() {
    let scratch = TempDir::new().unwrap();
    let project = scratch.path().join("empty-project");
    fs::create_dir_all(&project).unwrap();
    argosy_bin()
        .args(["index", "status"])
        .current_dir(&project)
        .env("XDG_STATE_HOME", xdg_state_home(&scratch))
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("default"))
        .stderr(predicate::str::contains("argosy init"));
}

/// Full round trip with the real backend: `build` then `status` then
/// `query --json`. Needs the ONNX model — the first run downloads ~90 MB
#[cfg(feature = "default-index")]
/// `cargo test --test cli -- --ignored index_build_status_query_round_trip`.
#[cfg(feature = "default-index")]
#[test]
#[ignore = "downloads the fastembed model (needs network on first run)"]
fn index_build_status_query_round_trip() {
    let scratch = TempDir::new().unwrap();
    let (project, xdg) = fixture_project(&scratch);
    let state_root = xdg.join("argosy");
    let store = pull::project_argosy_dir_at(&state_root, &project);

    argosy_bin()
        .args(["index", "build"])
        .current_dir(&project)
        .env("XDG_STATE_HOME", &xdg)
        .assert()
        .success()
        .stdout(predicate::str::contains("index"))
        .stdout(predicate::str::contains("fastembed/"));

    argosy_bin()
        .args(["index", "status"])
        .current_dir(&project)
        .env("XDG_STATE_HOME", &xdg)
        .assert()
        .success()
        .stdout(predicate::str::contains("model: fastembed/"))
        .stdout(predicate::str::contains("acme-billing/document: 3"))
        .stdout(predicate::str::contains("up to date"));

    // A second build is incremental: everything unchanged.
    argosy_bin()
        .args(["index", "build"])
        .current_dir(&project)
        .env("XDG_STATE_HOME", &xdg)
        .assert()
        .success()
        .stdout(predicate::str::contains("0 upserted, 0 removed"));

    // A second checkout in the project's store joins the index
    // automatically (no --import): rebuilding discovers it.
    let vendor = store.join("vendor-b");
    fs::create_dir_all(vendor.join("document")).unwrap();
    fs::write(
        vendor.join("argosy.md"),
        "---\ntype: Argosy Manifest\nname: vendor-b\nargosy_version: \"1.0.0\"\n---\n# vendor-b\n",
    )
    .unwrap();
    fs::write(
        vendor.join("document/spec.md"),
        "---\ntype: Note\ndescription: Vendored spec.\n---\nSpec content.\n",
    )
    .unwrap();
    argosy_bin()
        .args(["index", "build"])
        .current_dir(&project)
        .env("XDG_STATE_HOME", &xdg)
        .assert()
        .success()
        .stdout(predicate::str::contains("1 upserted, 0 removed"));
    argosy_bin()
        .args(["index", "status"])
        .current_dir(&project)
        .env("XDG_STATE_HOME", &xdg)
        .assert()
        .success()
        .stdout(predicate::str::contains("vendor-b/document: 1"));

    let output = argosy_bin()
        .args(["--json", "index", "query", "caching decisions", "-k", "3"])
        .current_dir(&project)
        .env("XDG_STATE_HOME", &xdg)
        .assert()
        .success()
        .get_output()
        .clone();
    let hits: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let hits = hits.as_array().unwrap();
    assert!(!hits.is_empty(), "a semantic query finds something");
    assert!(hits[0]["score"].is_number());
    assert_eq!(hits[0]["concept"]["argosy"], "acme-billing");

    // Unknown argosy names are an error, not silent emptiness.
    argosy_bin()
        .args(["index", "query", "caching", "--argosy", "no-such-argosy"])
        .current_dir(&project)
        .env("XDG_STATE_HOME", &xdg)
        .assert()
        .failure()
        .code(1);
}

// ------------------------------------------------------------------- agent
