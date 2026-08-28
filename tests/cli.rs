//! Integration tests for the `argosy` binary (doc 09 §2.3). These drive the
//! compiled binary, not library calls: argument parsing, exit codes, the
//! stdout/stderr split, and the `--json` schema are the contract under test.
//! Everything here runs offline; the real-backend index tests that need the
//! downloaded ONNX model are `#[ignore]`d like doc 07's backend tests.

use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

fn argosy_bin() -> Command {
    Command::cargo_bin("argosy").expect("binary builds with the package")
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Small recursive copy (fixtures are tiny).
fn copy_dir(source: &Path, dest: &Path) {
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
/// index tests start from a clean slate and mutating tests never touch the
/// checked-in fixtures.
fn fixture_copy(name: &str, scratch: &TempDir) -> PathBuf {
    let dest = scratch.path().join(name);
    copy_dir(&fixture(name), &dest);
    let cache = dest.join(".argosy");
    if cache.exists() {
        fs::remove_dir_all(&cache).unwrap();
    }
    dest
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
    // fixture's memory-present STR-9 info note); exit 0 means: no errors.
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

// ----------------------------------------------------------------- package

#[test]
fn package_to_directory_copies_the_bundle() {
    let scratch = TempDir::new().unwrap();
    let dest = scratch.path().join("out");
    argosy_bin()
        .args([
            "package",
            fixture("valid-acme-billing").to_str().unwrap(),
            dest.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("packaged acme-billing 0.3.1"));
    assert!(dest.join("argosy.md").is_file());
    assert!(dest.join("document/architecture.md").is_file());
    // The memory guarantee and default index exclusion, visible on disk.
    assert!(!dest.join("memory").exists());
    assert!(!dest.join(".argosy").exists());
}

#[test]
fn package_memory_exclusion_warns_even_under_quiet() {
    let scratch = TempDir::new().unwrap();
    let dest = scratch.path().join("out");
    argosy_bin()
        .args([
            "-q",
            "package",
            fixture("valid-acme-billing").to_str().unwrap(),
            dest.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("memory"));
}

#[test]
fn package_json_still_warns_on_stderr() {
    // `--json` governs stdout; the DIST-4 safeguard must never be silent.
    let scratch = TempDir::new().unwrap();
    let dest = scratch.path().join("out");
    argosy_bin()
        .args([
            "--json",
            "package",
            fixture("valid-acme-billing").to_str().unwrap(),
            dest.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("memory"));
}

#[test]
fn package_tar_gz_produces_a_gzip_archive() {
    let scratch = TempDir::new().unwrap();
    let dest = scratch.path().join("bundle.tar.gz");
    argosy_bin()
        .args([
            "package",
            "--format",
            "tar.gz",
            fixture("valid-acme-billing").to_str().unwrap(),
            dest.to_str().unwrap(),
        ])
        .assert()
        .success();
    let bytes = fs::read(&dest).unwrap();
    assert_eq!(&bytes[..2], &[0x1f, 0x8b], "gzip magic");
}

#[test]
fn package_include_index_ships_the_argosy_cache() {
    let scratch = TempDir::new().unwrap();
    let dest = scratch.path().join("with-index");
    argosy_bin()
        .args([
            "package",
            "--include-index",
            fixture("valid-acme-billing").to_str().unwrap(),
            dest.to_str().unwrap(),
        ])
        .assert()
        .success();
    assert!(dest.join(".argosy/index.db").is_file());
}

#[test]
fn package_of_broken_bundle_fails_with_validation_errors() {
    let scratch = TempDir::new().unwrap();
    let dest = scratch.path().join("out");
    argosy_bin()
        .args([
            "package",
            fixture("missing-manifest").to_str().unwrap(),
            dest.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("STR-2"));
    assert!(!dest.exists(), "a failed package leaves no artifact");
}

// ----------------------------------------------------------------- convert

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

// ------------------------------------------------------------------- index
// All index tests are gated like doc 07: without the `default-index`
// feature the binary refuses the subcommand entirely.

#[cfg(feature = "default-index")]
#[test]
fn index_status_reports_a_missing_index_without_creating_one() {
    let scratch = TempDir::new().unwrap();
    let project = fixture_copy("valid-acme-billing", &scratch);
    argosy_bin()
        .args(["index", project.to_str().unwrap(), "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("no index at"));
    assert!(!project.join(".argosy").exists(), "status never writes");
}

#[test]
fn help_documents_the_package_memory_guarantee() {
    argosy_bin()
        .args(["package", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("memory"))
        .stdout(predicate::str::contains("DIST-3"));
}

#[cfg(feature = "default-index")]
#[test]
fn help_documents_the_index_default_location_and_model_download() {
    argosy_bin()
        .args(["index", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(".argosy/index.db"));

    argosy_bin()
        .args(["index", "some-root", "build", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("90 MB"))
        .stdout(predicate::str::contains("download"));
}

/// Full round trip with the real backend: `build` then `status` then
/// `query --json`. Needs the ONNX model — the first run downloads ~90 MB
/// into the fastembed cache. Run once locally:
/// `cargo test --test cli -- --ignored index_build_status_query_round_trip`.
#[cfg(feature = "default-index")]
#[test]
#[ignore = "downloads the fastembed model (needs network on first run)"]
fn index_build_status_query_round_trip() {
    let scratch = TempDir::new().unwrap();
    let project = fixture_copy("valid-acme-billing", &scratch);

    argosy_bin()
        .args(["index", project.to_str().unwrap(), "build"])
        .assert()
        .success()
        .stdout(predicate::str::contains("index"))
        .stdout(predicate::str::contains("fastembed/"));

    argosy_bin()
        .args(["index", project.to_str().unwrap(), "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("model: fastembed/"))
        .stdout(predicate::str::contains("acme-billing/document: 3"))
        .stdout(predicate::str::contains("up to date"));

    // A second build is incremental: everything unchanged.
    argosy_bin()
        .args(["index", project.to_str().unwrap(), "build"])
        .assert()
        .success()
        .stdout(predicate::str::contains("0 upserted, 0 removed"));

    let output = argosy_bin()
        .args([
            "--json",
            "index",
            project.to_str().unwrap(),
            "query",
            "caching decisions",
            "-k",
            "3",
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    let hits: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let hits = hits.as_array().unwrap();
    assert!(!hits.is_empty(), "a semantic query finds something");
    assert!(hits[0]["score"].is_number());
    assert_eq!(hits[0]["concept"]["argosy"], "acme-billing");

    // Unknown argosy names are an error (`QRY-2`), not silent emptiness.
    argosy_bin()
        .args([
            "index",
            project.to_str().unwrap(),
            "query",
            "caching",
            "--argosy",
            "no-such-argosy",
        ])
        .assert()
        .failure()
        .code(1);
}
