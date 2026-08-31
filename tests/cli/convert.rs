//! `convert styleguide` tests.

use std::fs;

use predicates::prelude::*;

use tempfile::TempDir;

use argosy::pull;

use super::common::*;

const GOOD_RULES: &str = "\
- id: no-unwrap-in-prod
  description: Do not call unwrap outside tests.
  language: rust
  category: error-handling
  priority: error
  pattern: \".unwrap()\"
  good:
    - \"let value = maybe()?;\"
  bad: \"let value = maybe.unwrap();\"
- id: minimal-rule
  description: A rule with only the required fields.
";

const MAPPING_RULES: &str = "\
rules:
  - id: no-eval
    description: Never evaluate strings as code.
    language: python
    priority: warn
";

const MALFORMED_RULES: &str = "\
- id: no-description-here
  language: rust
- id: bad-priority
  description: Priority outside the error/warn/info vocabulary.
  priority: blocker
";

#[test]
fn convert_styleguide_imports_then_skips_on_rerun() {
    let scratch = TempDir::new().unwrap();
    let target = fixture_copy("valid-acme-billing", &scratch);
    let yaml_dir = scratch.path().join("yaml");
    fs::create_dir_all(&yaml_dir).unwrap();
    fs::write(yaml_dir.join("rust.yaml"), GOOD_RULES).unwrap();
    fs::write(yaml_dir.join("python.yaml"), MAPPING_RULES).unwrap();

    let yaml = yaml_dir.to_str().unwrap().to_string();
    let argosy_path = target.to_str().unwrap().to_string();

    argosy_bin()
        .args(["convert", "styleguide", &yaml, &argosy_path])
        .assert()
        .success()
        .stdout(predicate::str::contains("written: 3 rule(s)"))
        .stdout(predicate::str::contains("skipped (existing): 0"));
    assert!(
        target
            .join("styleguide/rust/error-handling/no-unwrap-in-prod.md")
            .is_file()
    );

    // Re-runnable: everything already exists, nothing is overwritten.
    argosy_bin()
        .args(["convert", "styleguide", &yaml, &argosy_path])
        .assert()
        .success()
        .stdout(predicate::str::contains("written: 0 rule(s)"))
        .stdout(predicate::str::contains("skipped (existing): 3"));
}

#[test]
fn convert_styleguide_fails_with_findings_on_malformed_rules() {
    let scratch = TempDir::new().unwrap();
    let target = fixture_copy("valid-acme-billing", &scratch);
    let yaml_dir = scratch.path().join("yaml");
    fs::create_dir_all(&yaml_dir).unwrap();
    fs::write(yaml_dir.join("broken.yaml"), MALFORMED_RULES).unwrap();

    argosy_bin()
        .args([
            "convert",
            "styleguide",
            yaml_dir.to_str().unwrap(),
            target.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("no-description-here"))
        .stdout(predicate::str::contains("bad-priority"));
}

#[test]
fn convert_styleguide_without_yaml_files_warns_on_stderr() {
    // Regression: a directory with no .yaml/.yml files (typo'd path) used
    // to look like a clean no-op success.
    let scratch = TempDir::new().unwrap();
    let target = fixture_copy("valid-acme-billing", &scratch);
    let yaml_dir = scratch.path().join("yaml");
    fs::create_dir_all(&yaml_dir).unwrap();
    fs::write(yaml_dir.join("rules.yaml.bak"), "id: x").unwrap();

    argosy_bin()
        .args([
            "convert",
            "styleguide",
            yaml_dir.to_str().unwrap(),
            target.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("no .yaml or .yml files found"))
        .stdout(predicate::str::contains("written: 0 rule(s)"));
}

#[test]
fn convert_styleguide_defaults_to_project_argosy() {
    let scratch = TempDir::new().unwrap();
    // A project in the standard layout: rules land in the project's
    // `default` bundle under the state dir when no argosy path is passed.
    let project = scratch.path().join("project");
    fs::create_dir_all(&project).unwrap();
    let xdg = xdg_state_home(&scratch);
    let target = pull::project_argosy_dir_at(&xdg.join("argosy"), &project).join("default");
    copy_dir(&fixture("valid-acme-billing"), &target);
    let cache = target.join(".argosy");
    if cache.exists() {
        fs::remove_dir_all(&cache).unwrap();
    }
    let yaml_dir = scratch.path().join("yaml");
    fs::create_dir_all(&yaml_dir).unwrap();
    fs::write(yaml_dir.join("rust.yaml"), GOOD_RULES).unwrap();

    argosy_bin()
        .args(["convert", "styleguide", yaml_dir.to_str().unwrap()])
        .current_dir(&project)
        .env("XDG_STATE_HOME", &xdg)
        .assert()
        .success()
        .stdout(predicate::str::contains("written: 2 rule(s)"));

    assert!(
        target
            .join("styleguide/rust/error-handling/no-unwrap-in-prod.md")
            .is_file()
    );
    assert!(!project.join(".argosy").exists(), "tree stays argosy-free");
}

#[test]
fn convert_styleguide_without_project_argosy_fails() {
    let scratch = TempDir::new().unwrap();
    let project = scratch.path().join("project");
    fs::create_dir_all(&project).unwrap();
    let yaml_dir = scratch.path().join("yaml");
    fs::create_dir_all(&yaml_dir).unwrap();
    fs::write(yaml_dir.join("rust.yaml"), GOOD_RULES).unwrap();

    argosy_bin()
        .args(["convert", "styleguide", yaml_dir.to_str().unwrap()])
        .current_dir(&project)
        .env("XDG_STATE_HOME", xdg_state_home(&scratch))
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("argosy init"));
}

/// The index reconciles on every write, and an import is a bulk write:
/// after `convert styleguide` the new rules must be searchable without a
/// manual `index build`. Needs the ONNX model — run with
/// `cargo test --test cli -- --ignored convert_import_reconciles_the_index`.
#[cfg(feature = "default-index")]
#[test]
#[ignore = "downloads the fastembed model (needs network on first run)"]
fn convert_import_reconciles_the_index() {
    let scratch = TempDir::new().unwrap();
    let (project, xdg) = fixture_project(&scratch);

    argosy_bin()
        .args(["index", "build"])
        .current_dir(&project)
        .env("XDG_STATE_HOME", &xdg)
        .assert()
        .success();

    let yaml_dir = scratch.path().join("yaml");
    fs::create_dir_all(&yaml_dir).unwrap();
    fs::write(
        yaml_dir.join("rust.yaml"),
        "- id: never-panic-in-payment-code\n  description: Never call unwrap in payment code.\n",
    )
    .unwrap();
    argosy_bin()
        .args(["convert", "styleguide", yaml_dir.to_str().unwrap()])
        .current_dir(&project)
        .env("XDG_STATE_HOME", &xdg)
        .assert()
        .success()
        .stdout(predicate::str::contains("index reconciled: 1 upserted"));

    // Searchable immediately, no `index build` in between.
    let output = argosy_bin()
        .args(["--json", "index", "query", "never call unwrap", "-k", "5"])
        .current_dir(&project)
        .env("XDG_STATE_HOME", &xdg)
        .assert()
        .success()
        .get_output()
        .clone();
    let hits: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let found = hits
        .as_array()
        .unwrap()
        .iter()
        .any(|h| h["concept"]["id"]
            .as_str()
            .is_some_and(|id| id.contains("never-panic-in-payment-code")));
    assert!(found, "imported rule must be searchable: {hits}");
}

// -------------------------------------------------------------------- pull
