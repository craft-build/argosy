//! `index` tests (plus the ignored real-backend round trip).

use std::fs;
use std::path::Path;
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

/// Full round trip with the real backend: `build` then `status` then `query --json`.
/// The first run downloads ~90 MB of model weights.
#[cfg(feature = "default-index")]
#[cfg(feature = "default-index")]
#[test]
#[ignore = "downloads the tract model weights (needs network on first run)"]
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
        .stdout(predicate::str::contains("tract/"));

    argosy_bin()
        .args(["index", "status"])
        .current_dir(&project)
        .env("XDG_STATE_HOME", &xdg)
        .assert()
        .success()
        .stdout(predicate::str::contains("model: tract/"))
        .stdout(predicate::str::contains("acme-billing/document: 3"))
        .stdout(predicate::str::contains("up to date"));

    argosy_bin()
        .args(["index", "build"])
        .current_dir(&project)
        .env("XDG_STATE_HOME", &xdg)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "0 concept(s) (0 passage(s)) upserted, 0 removed",
        ));

    // A second checkout joins the index automatically (no --import): rebuilding discovers it.
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
        .stdout(predicate::str::contains(
            "1 concept(s) (1 passage(s)) upserted, 0 removed",
        ));
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

// --- index eval (lexical mode: no model, no network) ---

/// A scratch project whose config forces `search_mode = "lexical"`.
#[cfg(feature = "default-index")]
fn lexical_project(scratch: &TempDir) -> (std::path::PathBuf, std::path::PathBuf) {
    let (project, xdg) = fixture_project(scratch);
    let config_home = scratch.path().join("xdg-config");
    fs::create_dir_all(config_home.join("argosy")).unwrap();
    fs::write(
        config_home.join("argosy").join("index.bml"),
        "index {\n    search_mode = \"lexical\"\n}\n",
    )
    .unwrap();
    (project, xdg)
}

/// Writes an eval dataset under `scratch` and returns its path.
#[cfg(feature = "default-index")]
fn write_dataset(scratch: &TempDir, name: &str, body: &str) -> std::path::PathBuf {
    let path = scratch.path().join(name);
    fs::write(&path, body).unwrap();
    path
}

#[cfg(feature = "default-index")]
const PASSING_DATASET: &str = r#"{
    "k": [1, 5],
    "min": {"recall@5": 1.0, "mrr": 0.3},
    "queries": [
        {"query": "cache computed invoice totals",
         "expected": ["argosy://acme-billing/document/decisions/2026-05-caching"],
         "absent": ["argosy://acme-billing/document/rate-limit-behavior"]},
        {"query": "processor rate limit retries",
         "expected": ["argosy://acme-billing/document/rate-limit-behavior"]},
        {"query": "payment flow architecture",
         "expected": ["argosy://acme-billing/document/architecture"]}
    ]
}"#;

#[cfg(feature = "default-index")]
#[test]
fn index_eval_scores_passes_and_fails_its_floors_in_lexical_mode() {
    let scratch = TempDir::new().unwrap();
    let (project, xdg) = lexical_project(&scratch);
    argosy_bin()
        .args(["index", "build"])
        .current_dir(&project)
        .env("XDG_STATE_HOME", &xdg)
        .env("XDG_CONFIG_HOME", scratch.path().join("xdg-config"))
        .assert()
        .success();
    let dataset = write_dataset(&scratch, "passing.json", PASSING_DATASET);

    argosy_bin()
        .args(["index", "eval", dataset.to_str().unwrap()])
        .current_dir(&project)
        .env("XDG_STATE_HOME", &xdg)
        .env("XDG_CONFIG_HOME", scratch.path().join("xdg-config"))
        .assert()
        .success()
        .stdout(predicate::str::contains("recall@1"))
        .stdout(predicate::str::contains("recall@5    1.000"))
        .stdout(predicate::str::contains("mrr"))
        .stdout(predicate::str::contains("floor    recall@5"));

    // Two expected URIs for one query cap recall@1 at 0.5: the 0.9 floor
    // fails by arithmetic, not by ranking luck — and --report-only rescues it.
    let gated = write_dataset(
        &scratch,
        "gated.json",
        r#"{
            "k": [1],
            "min": {"recall@1": 0.9},
            "queries": [
                {"query": "cache computed invoice totals",
                 "expected": ["argosy://acme-billing/document/decisions/2026-05-caching",
                              "argosy://acme-billing/document/architecture"]}
            ]
        }"#,
    );
    for (extra, code) in [(vec![], 1), (vec!["--report-only"], 0)] {
        argosy_bin()
            .args(["index", "eval", gated.to_str().unwrap()])
            .args(&extra)
            .current_dir(&project)
            .env("XDG_STATE_HOME", &xdg)
            .env("XDG_CONFIG_HOME", scratch.path().join("xdg-config"))
            .assert()
            .code(code)
            .stderr(predicate::str::contains("below threshold"));
    }

    // Floor overrides replace the dataset's per-k values.
    argosy_bin()
        .args([
            "index",
            "eval",
            dataset.to_str().unwrap(),
            "--min-recall",
            "5=0.5",
            "--min-mrr",
            "0.1",
        ])
        .current_dir(&project)
        .env("XDG_STATE_HOME", &xdg)
        .env("XDG_CONFIG_HOME", scratch.path().join("xdg-config"))
        .assert()
        .success();
}

#[cfg(feature = "default-index")]
#[test]
fn index_eval_fails_loudly_on_labels_missing_from_the_index() {
    let scratch = TempDir::new().unwrap();
    let (project, xdg) = lexical_project(&scratch);
    argosy_bin()
        .args(["index", "build"])
        .current_dir(&project)
        .env("XDG_STATE_HOME", &xdg)
        .env("XDG_CONFIG_HOME", scratch.path().join("xdg-config"))
        .assert()
        .success();
    let dataset = write_dataset(
        &scratch,
        "ghost.json",
        r#"{
            "k": [5],
            "queries": [
                {"query": "anything",
                 "expected": ["argosy://acme-billing/document/ghost"]}
            ]
        }"#,
    );
    argosy_bin()
        .args(["index", "eval", dataset.to_str().unwrap()])
        .current_dir(&project)
        .env("XDG_STATE_HOME", &xdg)
        .env("XDG_CONFIG_HOME", scratch.path().join("xdg-config"))
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("not present in the index"))
        .stderr(predicate::str::contains(
            "argosy://acme-billing/document/ghost",
        ));
}

#[cfg(feature = "default-index")]
#[test]
fn index_eval_rejects_malformed_floors_and_missing_datasets() {
    let scratch = TempDir::new().unwrap();
    let (project, xdg) = lexical_project(&scratch);
    argosy_bin()
        .args(["index", "build"])
        .current_dir(&project)
        .env("XDG_STATE_HOME", &xdg)
        .env("XDG_CONFIG_HOME", scratch.path().join("xdg-config"))
        .assert()
        .success();
    let dataset = write_dataset(&scratch, "passing.json", PASSING_DATASET);

    argosy_bin()
        .args([
            "index",
            "eval",
            dataset.to_str().unwrap(),
            "--min-recall",
            "10",
        ])
        .current_dir(&project)
        .env("XDG_STATE_HOME", &xdg)
        .env("XDG_CONFIG_HOME", scratch.path().join("xdg-config"))
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("--min-recall expects <k>=<floor>"));

    argosy_bin()
        .args([
            "index",
            "eval",
            scratch.path().join("no-such.json").to_str().unwrap(),
        ])
        .current_dir(&project)
        .env("XDG_STATE_HOME", &xdg)
        .env("XDG_CONFIG_HOME", scratch.path().join("xdg-config"))
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("no-such.json"));
}

#[test]
fn index_eval_without_an_index_points_at_build() {
    let scratch = TempDir::new().unwrap();
    let (project, xdg) = fixture_project(&scratch);
    let dataset = write_dataset(&scratch, "empty.json", r#"{"k": [5], "queries": []}"#);
    argosy_bin()
        .args(["index", "eval", dataset.to_str().unwrap()])
        .current_dir(&project)
        .env("XDG_STATE_HOME", &xdg)
        .assert()
        .success()
        .stdout(predicate::str::contains("no index at"));
}

/// The checked-in dataset must always parse and validate — asserted against a
/// book it does not label, where the only way past parsing and validation is
/// the loud label-hygiene failure.
#[cfg(feature = "default-index")]
#[test]
fn the_repository_retrieval_dataset_parses_and_validates() {
    let scratch = TempDir::new().unwrap();
    let (project, xdg) = lexical_project(&scratch);
    argosy_bin()
        .args(["index", "build"])
        .current_dir(&project)
        .env("XDG_STATE_HOME", &xdg)
        .env("XDG_CONFIG_HOME", scratch.path().join("xdg-config"))
        .assert()
        .success();
    let dataset = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("evals")
        .join("retrieval.json");
    argosy_bin()
        .args(["index", "eval", dataset.to_str().unwrap()])
        .current_dir(&project)
        .env("XDG_STATE_HOME", &xdg)
        .env("XDG_CONFIG_HOME", scratch.path().join("xdg-config"))
        .assert()
        .failure()
        .code(1)
        // The acme-billing book holds none of the argosy-docs labels, so the
        // run must stop at label hygiene — never at a parse or validation error.
        .stderr(predicate::str::contains("not present in the index"));
}
