//! `init` and `validate` tests.

use std::fs;
use std::path::Path;
use tempfile::TempDir;

use predicates::prelude::*;

use super::common::*;

#[test]
fn init_creates_a_bundle_that_validates_clean() {
    let scratch = TempDir::new().unwrap();
    let target = scratch.path().join("fresh-bundle");
    argosy_bin()
        .args(["init", target.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("created fresh-bundle 0.1.0"));

    assert!(target.join("argosy.md").is_file());
    for ns in ["document", "skill", "memory", "styleguide"] {
        assert!(target.join(ns).is_dir());
    }
    let manifest = fs::read_to_string(target.join("argosy.md")).unwrap();
    assert!(manifest.contains("name: fresh-bundle"));

    // The created bundle passes its own validator.
    argosy_bin()
        .args(["validate", target.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("OK: fresh-bundle 0.1.0"));
}

#[test]
fn init_current_directory_and_json_output() {
    // No path: the project's `default` bundle under the state dir, named
    // after the project directory, keyed by its slug (name + path hash).
    let scratch = TempDir::new().unwrap();
    let target = scratch.path().join("cwd-test");
    fs::create_dir_all(&target).unwrap();
    let xdg = xdg_state_home(&scratch);
    let output = argosy_bin()
        .args(["--json", "init"])
        .current_dir(&target)
        .env("XDG_STATE_HOME", &xdg)
        .assert()
        .success()
        .get_output()
        .clone();
    let created: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(created["name"], "cwd-test");
    assert_eq!(created["argosy_version"], "0.1.0");
    let path = created["path"].as_str().unwrap();
    assert!(
        path.contains("argosy/projects/cwd-test-") && path.ends_with("/default"),
        "state-dir layout: {path}"
    );
    assert!(Path::new(path).join("argosy.md").is_file());
    // The project tree itself stays argosy-free.
    assert!(!target.join(".argosy").exists());

    // And the created bundle validates.
    argosy_bin()
        .args(["validate", path])
        .assert()
        .success()
        .stdout(predicate::str::contains("OK: cwd-test 0.1.0"));
}

#[test]
fn init_refuses_to_overwrite_an_existing_bundle() {
    let scratch = TempDir::new().unwrap();
    let target = scratch.path().join("twice");
    argosy_bin()
        .args(["init", target.to_str().unwrap()])
        .assert()
        .success();
    argosy_bin()
        .args(["init", target.to_str().unwrap()])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("already contains an argosy"));
}

// ---------------------------------------------------------------- validate

#[test]
fn validate_valid_fixture_exits_zero_with_ok_line() {
    argosy_bin()
        .args(["validate", fixture("valid-acme-billing").to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("OK: acme-billing 0.3.1"));
}

#[test]
fn validate_broken_fixtures_exit_one_with_requirement_ids() {
    let cases = [
        ("missing-manifest", "STR-2"),
        ("bad-semver", "STR-5"),
        ("untyped-concept", "DOC-1"),
        ("skill-missing-entry-point", "SKL-2"),
    ];
    for (name, requirement) in cases {
        argosy_bin()
            .args(["validate", fixture(name).to_str().unwrap()])
            .assert()
            .failure()
            .code(1)
            .stdout(predicate::str::contains(requirement));
    }
}

#[test]
fn validate_json_is_the_serialized_validation_report() {
    let output = argosy_bin()
        .args([
            "--json",
            "validate",
            fixture("missing-manifest").to_str().unwrap(),
        ])
        .assert()
        .failure()
        .code(1)
        .get_output()
        .clone();
    // The exact schema of `ValidationReport` in `src/bundle.rs`.
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let findings = report["findings"].as_array().unwrap();
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0]["severity"], "error");
    assert_eq!(findings[0]["id"], "STR-2");
    assert!(
        findings[0]["message"]
            .as_str()
            .unwrap()
            .contains("argosy.md")
    );

    let ok = argosy_bin()
        .args([
            "--json",
            "validate",
            fixture("valid-acme-billing").to_str().unwrap(),
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    let report: serde_json::Value = serde_json::from_slice(&ok.stdout).unwrap();
    // Conformant bundles still serialize their non-error findings (the
    // info note); exit 0 means: no errors.
    let findings = report["findings"].as_array().unwrap();
    assert!(
        findings.iter().all(|f| f["severity"] != "error"),
        "conformant fixture has no error findings: {findings:?}"
    );
}

#[test]
fn validate_namespace_skill_runs_only_skill_checks() {
    argosy_bin()
        .args([
            "validate",
            "--namespace",
            "skill",
            fixture("skill-missing-entry-point").to_str().unwrap(),
        ])
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("SKL-"))
        .stdout(predicate::str::contains("STR-").not());
}

#[test]
fn validate_namespace_skill_passes_on_valid_fixture() {
    argosy_bin()
        .args([
            "validate",
            "--namespace",
            "skill",
            fixture("valid-acme-billing").to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("OK: acme-billing"));
}

#[test]
fn validate_namespace_document_memory_keep_bundle_level_findings() {
    // Regression: scoped runs used to drop findings with no path, so a
    // bundle with no `argosy.md` validated "OK" under any namespace scope.
    for ns in ["document", "memory"] {
        argosy_bin()
            .args([
                "validate",
                "--namespace",
                ns,
                fixture("missing-manifest").to_str().unwrap(),
            ])
            .assert()
            .failure()
            .code(1)
            .stdout(predicate::str::contains("STR-2"));
    }
}

#[test]
fn validate_namespace_memory_scopes_path_findings() {
    // Path findings stay scoped: an untyped concept under document/ is not
    // reported by a memory-scoped run.
    argosy_bin()
        .args([
            "validate",
            "--namespace",
            "memory",
            fixture("untyped-concept").to_str().unwrap(),
        ])
        .assert()
        .success();
}

// ----------------------------------------------------------------- package
