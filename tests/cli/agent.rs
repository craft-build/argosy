//! `agent reviewer` tests.

use std::fs;
use tempfile::TempDir;

use predicates::prelude::*;

use super::common::*;

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
