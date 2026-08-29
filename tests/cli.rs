//! Integration tests for the `argosy` binary. These drive the
//! compiled binary, not library calls: argument parsing, exit codes, the
//! stdout/stderr split, and the `--json` schema are the contract under test.
//! Everything here runs offline; the real-backend index tests that need the
//! downloaded ONNX model are `#[ignore]`d like the backend tests.

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

/// A project in the standard layout: `<root>/.argosy/default` holding a
/// copy of the valid fixture (sans its bundled `.argosy/` placeholder).
#[cfg(feature = "default-index")] // only the gated index tests build projects
fn fixture_project(scratch: &TempDir) -> PathBuf {
    let project = scratch.path().join("project");
    let local = project.join(".argosy/default");
    copy_dir(&fixture("valid-acme-billing"), &local);
    let cache = local.join(".argosy");
    if cache.exists() {
        fs::remove_dir_all(&cache).unwrap();
    }
    project
}

/// Initializes `dir` as a git repo with one commit (for `pull` tests; git
/// needs no network for local clones).
fn git_commit_all(dir: &Path) {
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

// -------------------------------------------------------------------- init

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
    // No path: the project-local bundle at `.argosy/default`, named after
    // the project directory.
    let scratch = TempDir::new().unwrap();
    let target = scratch.path().join("cwd-test");
    fs::create_dir_all(&target).unwrap();
    let output = argosy_bin()
        .args(["--json", "init"])
        .current_dir(&target)
        .assert()
        .success()
        .get_output()
        .clone();
    let created: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(created["name"], "cwd-test");
    assert_eq!(created["argosy_version"], "0.1.0");
    assert!(target.join(".argosy/default/argosy.md").is_file());

    // And the project is then indexable in exactly that layout.
    argosy_bin()
        .args(["validate", target.join(".argosy/default").to_str().unwrap()])
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
    // A project in the standard layout: rules land in `.argosy/default`
    // when no argosy path is passed.
    let project = scratch.path().join("project");
    let target = project.join(".argosy/default");
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
        .assert()
        .success()
        .stdout(predicate::str::contains("written: 2 rule(s)"));

    assert!(
        target
            .join("styleguide/rust/error-handling/no-unwrap-in-prod.md")
            .is_file()
    );
}

#[test]
fn convert_styleguide_without_project_argosy_fails() {
    let scratch = TempDir::new().unwrap();
    let yaml_dir = scratch.path().join("yaml");
    fs::create_dir_all(&yaml_dir).unwrap();
    fs::write(yaml_dir.join("rust.yaml"), GOOD_RULES).unwrap();

    argosy_bin()
        .args(["convert", "styleguide", yaml_dir.to_str().unwrap()])
        .current_dir(scratch.path())
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains(".argosy/default"));
}

// -------------------------------------------------------------------- pull

#[test]
fn pull_clones_a_remote_bundle_into_the_project() {
    let scratch = TempDir::new().unwrap();
    let repo = fixture_copy("valid-acme-billing", &scratch);
    git_commit_all(&repo);
    let project = scratch.path().join("project");
    fs::create_dir_all(&project).unwrap();

    argosy_bin()
        .args(["pull", repo.to_str().unwrap(), "company-rules"])
        .current_dir(&project)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "pulled acme-billing 0.3.1 into .argosy/company-rules",
        ));
    assert!(project.join(".argosy/company-rules/argosy.md").is_file());

    // A checkout is never overwritten.
    argosy_bin()
        .args(["pull", repo.to_str().unwrap(), "company-rules"])
        .current_dir(&project)
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

    argosy_bin()
        .args(["pull", repo.to_str().unwrap(), "notargosy"])
        .current_dir(&project)
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("not an argosy"));
    assert!(!project.join(".argosy/notargosy").exists());
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
            .join(".local/state/argosy/shared-rules/argosy.md")
            .is_file()
    );
}

// ------------------------------------------------------------------- index
// All index tests are gated: without the `default-index`
// feature the binary refuses the subcommand entirely.

#[cfg(feature = "default-index")]
#[test]
fn index_status_reports_a_missing_index_without_creating_one() {
    let scratch = TempDir::new().unwrap();
    let project = fixture_project(&scratch);
    argosy_bin()
        .args(["index", "status"])
        .current_dir(&project)
        .assert()
        .success()
        .stdout(predicate::str::contains("no index at"));
    assert!(
        !project.join(".argosy/index.db").exists(),
        "status never writes"
    );
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
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains(".argosy/default"))
        .stderr(predicate::str::contains("argosy init"));
}

#[test]
fn help_documents_the_package_memory_guarantee() {
    argosy_bin()
        .args(["package", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("memory"))
        .stdout(predicate::str::contains("NEVER included"));
}

#[cfg(feature = "mcp")]
#[test]
fn help_documents_the_mcp_stdio_transport_and_model_download() {
    // The model download fires on `mcp` first runs too, not just
    // `index build` — the help must say so.
    argosy_bin()
        .args(["mcp", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("stdio"))
        .stdout(predicate::str::contains("90 MB"));
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
        .args(["index", "build", "--help"])
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
    let project = fixture_project(&scratch);

    argosy_bin()
        .args(["index", "build"])
        .current_dir(&project)
        .assert()
        .success()
        .stdout(predicate::str::contains("index"))
        .stdout(predicate::str::contains("fastembed/"));

    argosy_bin()
        .args(["index", "status"])
        .current_dir(&project)
        .assert()
        .success()
        .stdout(predicate::str::contains("model: fastembed/"))
        .stdout(predicate::str::contains("acme-billing/document: 3"))
        .stdout(predicate::str::contains("up to date"));

    // A second build is incremental: everything unchanged.
    argosy_bin()
        .args(["index", "build"])
        .current_dir(&project)
        .assert()
        .success()
        .stdout(predicate::str::contains("0 upserted, 0 removed"));

    // A second checkout in `.argosy/` joins the index automatically (no
    // --import): rebuilding discovers it.
    let vendor = project.join(".argosy/vendor-b");
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
        .assert()
        .success()
        .stdout(predicate::str::contains("1 upserted, 0 removed"));
    argosy_bin()
        .args(["index", "status"])
        .current_dir(&project)
        .assert()
        .success()
        .stdout(predicate::str::contains("vendor-b/document: 1"));

    let output = argosy_bin()
        .args(["--json", "index", "query", "caching decisions", "-k", "3"])
        .current_dir(&project)
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
        .assert()
        .failure()
        .code(1);
}

// ------------------------------------------------------------------- agent

#[test]
fn agent_reviewer_writes_each_harness_definition_from_cwd() {
    for (harness, rel) in [
        ("opencode", ".opencode/agents/reviewer.md"),
        ("claude", ".claude/agents/reviewer.md"),
        ("kiro-cli", ".kiro/agents/reviewer.md"),
    ] {
        let scratch = TempDir::new().unwrap();
        let project = scratch.path().join("project");
        fs::create_dir_all(&project).unwrap();

        argosy_bin()
            .args(["agent", "reviewer", harness])
            .current_dir(&project)
            .assert()
            .success()
            .stdout(predicate::str::contains("created reviewer agent for"))
            .stdout(predicate::str::contains(rel))
            // The MCP hint rides along on every fresh install.
            .stdout(predicate::str::contains("argosy mcp"));

        let path = project.join(rel);
        assert!(path.is_file(), "{harness}: definition written");
        let definition = fs::read_to_string(&path).unwrap();
        assert!(definition.starts_with("---\n"), "{harness}: frontmatter");
        // The shared reviewer system prompt body.
        assert!(definition.contains("You are a code reviewer"));
        assert!(definition.contains("**P0 - Critical**"));
        assert!(definition.contains("approve_with_nits"));
    }
}

#[test]
fn agent_reviewer_refuses_existing_then_force_replaces() {
    let scratch = TempDir::new().unwrap();
    let project = scratch.path().join("project");
    fs::create_dir_all(&project).unwrap();
    let path = project.join(".opencode/agents/reviewer.md");

    argosy_bin()
        .args(["agent", "reviewer", "opencode"])
        .current_dir(&project)
        .assert()
        .success();
    fs::write(&path, "user edits\n").unwrap();

    // Without --force the user's file is an error, and it stays untouched.
    argosy_bin()
        .args(["agent", "reviewer", "opencode"])
        .current_dir(&project)
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("already exists"));
    assert_eq!(fs::read_to_string(&path).unwrap(), "user edits\n");

    // With --force the definition is regenerated.
    argosy_bin()
        .args(["agent", "reviewer", "opencode", "--force"])
        .current_dir(&project)
        .assert()
        .success()
        .stdout(predicate::str::contains("replaced reviewer agent"));
    assert!(
        fs::read_to_string(&path)
            .unwrap()
            .contains("mode: subagent")
    );
}

#[test]
fn agent_reviewer_json_is_the_setup_report() {
    let scratch = TempDir::new().unwrap();
    let project = scratch.path().join("project");
    fs::create_dir_all(&project).unwrap();
    let output = argosy_bin()
        .args(["--json", "agent", "reviewer", "kiro-cli"])
        .current_dir(&project)
        .assert()
        .success()
        .get_output()
        .clone();

    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["harness"], "kiro-cli");
    assert_eq!(report["overwritten"], false);
    assert!(
        report["path"]
            .as_str()
            .is_some_and(|p| p.ends_with(".kiro/agents/reviewer.md"))
    );
    // The human-only MCP hint never pollutes the JSON contract on stdout;
    // notes go to stdout only in human mode, so JSON stdout stays pure.
    assert!(!String::from_utf8_lossy(&output.stdout).contains("note:"));
}

#[test]
fn agent_reviewer_unknown_harness_is_a_usage_error() {
    let scratch = TempDir::new().unwrap();
    argosy_bin()
        .args(["agent", "reviewer", "cursor"])
        .current_dir(scratch.path())
        .assert()
        .failure()
        .code(2);
    // Nothing was written for the rejected harness.
    assert!(!scratch.path().join(".cursor").exists());
}
