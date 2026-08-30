//! `package` tests.

use std::fs;
use tempfile::TempDir;

use predicates::prelude::*;

use super::common::*;

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
    // `--json` governs stdout; the safeguard must never be silent.
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
    let source = fixture_copy("valid-acme-billing", &scratch);
    // `--include-index` ships a pre-built cache verbatim; it does not build
    // one. The bundle keeps no `.argosy/` runtime state in git (see
    // .gitignore), so plant a stand-in file in the copy: any bytes will do.
    fs::create_dir_all(source.join(".argosy")).unwrap();
    fs::write(source.join(".argosy/index.db"), "sqlite bytes").unwrap();
    let dest = scratch.path().join("with-index");
    argosy_bin()
        .args([
            "package",
            "--include-index",
            source.to_str().unwrap(),
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
