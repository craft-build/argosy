//! Tests for Craft YAML styleguide import.

use super::*;

#[test]
fn import_honors_craft_metadata_block_and_nested_examples() {
    let dir = TempDir::new().unwrap();
    let local = import_fixture(&dir);
    let yaml_dir = dir.path().join("yaml");
    write(&yaml_dir, "naming.yaml", CRAFT_SCHEMA_RULES);

    let report = import_styleguide_yaml(&local, &yaml_dir).unwrap();
    assert_eq!(report.written, 3);
    assert!(report.findings.is_empty(), "{:?}", report.findings);

    // File-level metadata facets place the rule and land in frontmatter.
    let path = local
        .root()
        .join("styleguide/rust/naming/SNAKE-CASE-VARS.md");
    assert!(path.is_file());
    let concept = Concept::from_file(&path).unwrap();
    assert_eq!(concept.get_str("language"), Some("rust"));
    assert_eq!(concept.get_str("category"), Some("naming"));
    assert_eq!(concept.get_str("rule_id"), Some("SNAKE-CASE-VARS"));
    // Schema `warning` canonicalizes to `warn`.
    assert_eq!(concept.get_str("priority"), Some("warn"));
    assert_eq!(concept.get_str("pattern"), Some("^[a-z][a-z0-9_]*$"));
    assert_eq!(concept.tags(), vec!["naming", "convention"]);
    // Nested examples become the body sections.
    assert!(concept.body().contains("## Good"));
    assert!(concept.body().contains("- let my_variable = 5;"));
    assert!(concept.body().contains("## Bad"));
    assert!(concept.body().contains("- let MyVariable = 5;"));

    // A rule's own facets override the file's metadata defaults.
    let path = local
        .root()
        .join("styleguide/rust-2021/style/OVERRIDE-EXPLICIT-FACETS.md");
    assert!(path.is_file());
    let concept = Concept::from_file(&path).unwrap();
    assert_eq!(concept.get_str("language"), Some("rust-2021"));
    assert_eq!(concept.get_str("category"), Some("style"));

    // `hint` is valid schema vocabulary and passes through untouched.
    let path = local
        .root()
        .join("styleguide/rust/naming/HINT-PRIORITY-KEPT.md");
    let concept = Concept::from_file(&path).unwrap();
    assert_eq!(concept.get_str("priority"), Some("hint"));

    assert_eq!(error_findings(&local).len(), 0);
}

#[test]
fn import_reports_malformed_metadata_without_aborting() {
    let dir = TempDir::new().unwrap();
    let local = import_fixture(&dir);
    let yaml_dir = dir.path().join("yaml");
    write(
        &yaml_dir,
        "broken-metadata.yaml",
        "metadata: not-a-mapping\nrules:\n  - id: orphaned-rule\n    description: Still imports.\n",
    );
    write(
        &yaml_dir,
        "numeric-metadata.yaml",
        "metadata:\n  language: 7\nrules:\n  - id: numeric-language\n    description: Still imports.\n",
    );

    let report = import_styleguide_yaml(&local, &yaml_dir).unwrap();
    assert_eq!(report.written, 2);
    let messages: Vec<&str> = report.findings.iter().map(|f| f.message.as_str()).collect();
    assert!(
        messages
            .iter()
            .any(|m| m.contains("`metadata` must be a mapping")),
        "{messages:?}"
    );
    assert!(
        messages
            .iter()
            .any(|m| m.contains("`metadata.language` must be a string")),
        "{messages:?}"
    );
    // The rules still landed, just without the broken file-level facets.
    assert!(
        local
            .root()
            .join("styleguide/general/misc/orphaned-rule.md")
            .is_file()
    );
    assert!(
        local
            .root()
            .join("styleguide/general/misc/numeric-language.md")
            .is_file()
    );
    assert_eq!(error_findings(&local).len(), 0);
}

const BROKEN_RULES: &str = "\
- id: no-description-here
  language: rust
- id: \"bad:colon\"
  description: This id cannot become a filename.
- id: bad-priority
  description: Priority outside the error/warn/info vocabulary.
  priority: blocker
- id: numeric-priority
  description: Non-string scalars in string fields are not silently dropped.
  priority: 1
";

#[test]
fn import_converts_craft_rulesets_into_styleguide_concepts() {
    let dir = TempDir::new().unwrap();
    let local = import_fixture(&dir);
    let yaml_dir = dir.path().join("yaml");
    write(&yaml_dir, "rust.yaml", RUST_RULES);
    write(&yaml_dir, "python.yml", MAPPING_RULES);

    let report = import_styleguide_yaml(&local, &yaml_dir).unwrap();
    assert_eq!(report.written, 3);
    assert!(report.skipped_existing.is_empty());
    assert!(report.findings.is_empty(), "{:?}", report.findings);

    // Locked path: language + category + rule id as the filename.
    let full = local
        .root()
        .join("styleguide/rust/error-handling/no-unwrap-in-prod.md");
    assert!(full.is_file());
    let concept = Concept::from_file(&full).unwrap();
    assert_eq!(concept.concept_type(), Some("Styleguide Rule"));
    assert_eq!(concept.get_str("language"), Some("rust"));
    assert_eq!(concept.get_str("category"), Some("error-handling"));
    assert_eq!(concept.get_str("rule_id"), Some("no-unwrap-in-prod"));
    assert_eq!(concept.get_str("priority"), Some("error"));
    assert_eq!(concept.get_str("pattern"), Some(".unwrap()"));
    let body = concept.body();
    assert!(body.contains("## Good"));
    assert!(body.contains("- let value = maybe()?;"));
    assert!(body.contains("## Bad"));
    assert!(body.contains("let value = maybe.unwrap();"));

    // Missing facets fall back to general/misc; single-string examples
    // stay unbulleted.
    let minimal = local.root().join("styleguide/general/misc/minimal-rule.md");
    let concept = Concept::from_file(&minimal).unwrap();
    assert_eq!(
        concept.get_str("description"),
        Some("A rule with only the required fields.")
    );
    assert!(!concept.body().contains("## Good"));

    let eval = local.root().join("styleguide/python/misc/no-eval.md");
    assert!(eval.is_file());
    let concept = Concept::from_file(&eval).unwrap();
    assert!(concept.body().contains("- eval(text)"));

    // The import produced conformant rules end to end.
    assert_eq!(error_findings(&local).len(), 0);
}

#[test]
fn import_is_additive_and_rerunnable() {
    let dir = TempDir::new().unwrap();
    let local = import_fixture(&dir);
    let yaml_dir = dir.path().join("yaml");
    write(&yaml_dir, "rust.yaml", RUST_RULES);

    import_styleguide_yaml(&local, &yaml_dir).unwrap();
    let second = import_styleguide_yaml(&local, &yaml_dir).unwrap();
    assert_eq!(second.written, 0);
    assert_eq!(
        second.skipped_existing,
        vec!["no-unwrap-in-prod".to_string(), "minimal-rule".to_string()]
    );
}

#[test]
fn import_collects_bad_rules_as_findings_without_aborting() {
    let dir = TempDir::new().unwrap();
    let local = import_fixture(&dir);
    let yaml_dir = dir.path().join("yaml");
    write(&yaml_dir, "broken.yaml", BROKEN_RULES);
    write(&yaml_dir, "python.yml", MAPPING_RULES);

    let report = import_styleguide_yaml(&local, &yaml_dir).unwrap();
    assert_eq!(report.written, 1, "good rules still land");
    assert_eq!(report.findings.len(), 4);
    let messages: Vec<&str> = report.findings.iter().map(|f| f.message.as_str()).collect();
    assert!(
        messages
            .iter()
            .any(|m| m.contains("no-description-here") && m.contains("description")),
        "{messages:?}"
    );
    assert!(
        messages.iter().any(|m| m.contains("bad:colon")),
        "{messages:?}"
    );
    assert!(
        messages
            .iter()
            .any(|m| m.contains("bad-priority") && m.contains("error/warn/warning/info/hint")),
        "{messages:?}"
    );
    assert!(
        messages
            .iter()
            .any(|m| m.contains("numeric-priority") && m.contains("must be a string")),
        "{messages:?}"
    );
    assert!(error_findings(&local).is_empty());
}

#[cfg(unix)]
#[test]
fn import_records_unreadable_files_as_findings() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new().unwrap();
    let local = import_fixture(&dir);
    let yaml_dir = dir.path().join("yaml");
    write(&yaml_dir, "locked.yaml", RUST_RULES);
    fs::set_permissions(
        yaml_dir.join("locked.yaml"),
        fs::Permissions::from_mode(0o000),
    )
    .unwrap();
    if fs::read_to_string(yaml_dir.join("locked.yaml")).is_ok() {
        // Running as root: permission bits don't gate reads, so the
        // unreadable-file path cannot be exercised in this environment.
        return;
    }
    write(&yaml_dir, "python.yml", MAPPING_RULES);

    let report = import_styleguide_yaml(&local, &yaml_dir).unwrap();
    assert_eq!(report.written, 1, "the batch must not abort");
    assert_eq!(report.findings.len(), 1);
    assert!(
        report.findings[0].message.contains("failed to read"),
        "{}",
        report.findings[0].message
    );
}
