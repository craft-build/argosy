//! Unit tests for bundle opening and validation.

use std::path::{Path, PathBuf};

use semver::Version;
use yaml_serde::Value;

use crate::concept::Concept;
use crate::error::Error;

use super::*;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn error_ids(report: &ValidationReport) -> Vec<Option<&'static str>> {
    report.errors().map(|f| f.id).collect()
}

#[test]
fn open_valid_fixture_reads_manifest_fields() {
    let argosy = Argosy::open(fixture("valid-acme-billing")).unwrap();
    let manifest = argosy.manifest();
    assert_eq!(manifest.name(), "acme-billing");
    assert_eq!(manifest.argosy_version(), &Version::new(0, 3, 1));
    assert_eq!(manifest.okf_version(), Some("0.2"));
    assert_eq!(
        manifest.description(),
        Some("Knowledge, skills, and memory for the ACME billing service.")
    );
}

#[test]
fn open_valid_fixture_lists_all_reserved_and_custom_namespaces() {
    let argosy = Argosy::open(fixture("valid-acme-billing")).unwrap();
    let present = argosy.namespaces_present();
    assert_eq!(
        present,
        vec![
            Namespace::Document,
            Namespace::Skill,
            Namespace::Memory,
            Namespace::Styleguide,
            Namespace::Custom("roadmap".to_string()),
        ]
    );
    // Reserved namespaces map back to directories; absent handling works.
    assert!(argosy.namespace_dir(&Namespace::Skill).is_some());
    assert!(
        argosy
            .namespace_dir(&Namespace::Custom("nope".to_string()))
            .is_none()
    );
}

#[test]
fn valid_fixture_is_conformant_with_no_errors() {
    let report = Argosy::validate(fixture("valid-acme-billing"));
    assert!(report.is_conformant());
    assert_eq!(report.errors().count(), 0);
    // `memory/` presence is surfaced as an info note, not an issue.
    let infos: Vec<_> = report
        .findings()
        .iter()
        .filter(|f| f.severity == Severity::Info)
        .collect();
    assert_eq!(infos.len(), 1);
    assert_eq!(infos[0].id, Some("STR-9"));
}

#[test]
fn unknown_manifest_keys_are_retained_without_findings() {
    let fixture = fixture("valid-acme-billing");
    let argosy = Argosy::open(&fixture).unwrap();
    // Unknown manifest keys parse fine and are retained.
    assert_eq!(
        argosy
            .manifest()
            .extra()
            .get("x-team")
            .and_then(Value::as_str),
        Some("payments")
    );
    assert!(argosy.manifest().extra().contains_key("generated"));
    // Known-but-unconsumed OKF fields stay too; consumed ones don't.
    assert!(argosy.manifest().extra().contains_key("tags"));
    assert!(!argosy.manifest().extra().contains_key("name"));

    let report = Argosy::validate(&fixture);
    assert!(
        report
            .findings()
            .iter()
            .all(|f| !f.message.contains("x-team"))
    );
}

#[test]
fn concepts_skill_is_sorted_and_excludes_listing_files() {
    let argosy = Argosy::open(fixture("valid-acme-billing")).unwrap();
    let skills = argosy.concepts(&Namespace::Skill).unwrap();
    let ids: Vec<_> = skills.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(
        ids,
        vec![
            "skill/reconcile-ledger",
            "skill/rotate-api-keys/references/checklist",
            "skill/rotate-api-keys/rotate-api-keys",
        ]
    );
    let sorted = ids.is_sorted();
    assert!(sorted);
}

#[test]
fn concepts_of_absent_namespace_is_empty_not_error() {
    let argosy = Argosy::open(fixture("valid-acme-billing")).unwrap();
    assert!(
        argosy
            .concepts(&Namespace::Custom("absent".to_string()))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn dot_argosy_index_directory_is_ignored_everywhere() {
    let fixture = fixture("valid-acme-billing");
    let argosy = Argosy::open(&fixture).unwrap();
    assert!(
        !argosy
            .namespaces_present()
            .contains(&Namespace::Custom(".argosy".to_string()))
    );
    let report = Argosy::validate(&fixture);
    assert!(report.findings().iter().all(|f| {
        !f.path
            .as_ref()
            .is_some_and(|p| p.to_string_lossy().contains(".argosy"))
    }));
}

#[test]
fn minimal_manifest_warns_about_should_fields_but_still_opens() {
    let fixture = fixture("minimal-manifest");
    Argosy::open(&fixture).unwrap();
    let report = Argosy::validate(&fixture);
    assert!(report.is_conformant());
    let warnings: Vec<_> = report.warnings().collect();
    assert_eq!(warnings.len(), 2);
    assert!(warnings.iter().any(|f| f.message.contains("okf_version")));
    assert!(warnings.iter().any(|f| f.message.contains("description")));
    // The SHOULD-level findings are traceable by ID like every other.
    assert!(warnings.iter().all(|f| f.id == Some("§4.2")));
}

#[test]
fn missing_manifest_is_str2_error_and_open_fails() {
    let fixture = fixture("missing-manifest");
    let report = Argosy::validate(&fixture);
    assert_eq!(error_ids(&report), vec![Some("STR-2")]);
    assert!(Argosy::open(&fixture).is_err());
}

#[test]
fn wrong_manifest_type_is_str5_error_and_open_fails() {
    let fixture = fixture("wrong-manifest-type");
    let report = Argosy::validate(&fixture);
    assert!(error_ids(&report).contains(&Some("STR-5")));
    assert!(Argosy::open(&fixture).is_err());
}

#[test]
fn manifest_without_frontmatter_is_str4_error_and_open_fails() {
    let fixture = fixture("manifest-no-frontmatter");
    let report = Argosy::validate(&fixture);
    assert_eq!(error_ids(&report), vec![Some("STR-4")]);
    assert!(Argosy::open(&fixture).is_err());
}

#[test]
fn nested_argosy_manifest_is_str3_error() {
    let report = Argosy::validate(fixture("nested-argosy"));
    assert_eq!(error_ids(&report), vec![Some("STR-3")]);
}

#[test]
fn argosy_md_used_as_ordinary_concept_is_str3_error() {
    let report = Argosy::validate(fixture("argosy-as-concept"));
    assert_eq!(error_ids(&report), vec![Some("STR-3")]);
}

#[test]
fn malformed_semver_is_str5_error_and_open_fails() {
    let fixture = fixture("bad-semver");
    let report = Argosy::validate(&fixture);
    assert!(error_ids(&report).contains(&Some("STR-5")));
    assert!(Argosy::open(&fixture).is_err());
}

#[test]
fn reserved_namespace_name_as_toplevel_file_is_str7_error() {
    let report = Argosy::validate(fixture("reserved-as-file-document"));
    assert_eq!(error_ids(&report), vec![Some("STR-7")]);
}

#[test]
fn reserved_filename_as_directory_is_str11_error() {
    let report = Argosy::validate(fixture("index-md-as-dir"));
    assert_eq!(error_ids(&report), vec![Some("STR-11")]);
}

#[test]
fn untyped_document_concept_is_doc1_error() {
    let report = Argosy::validate(fixture("untyped-concept"));
    assert_eq!(error_ids(&report), vec![Some("DOC-1")]);
}

#[test]
fn validate_on_non_directory_root_is_str1_error() {
    let file = fixture("missing-manifest").join("document/note.md");
    let report = Argosy::validate(&file);
    assert_eq!(error_ids(&report), vec![Some("STR-1")]);

    let missing = fixture("does-not-exist");
    let report = Argosy::validate(&missing);
    assert_eq!(error_ids(&report), vec![Some("STR-1")]);
    assert!(Argosy::open(&missing).is_err());
}

#[test]
fn report_display_renders_one_finding_per_line_with_id_and_path() {
    let rendered = Argosy::validate(fixture("untyped-concept")).to_string();
    let line = rendered.trim_end();
    assert!(
        line.starts_with("[ERROR DOC-1] document/untyped.md: "),
        "unexpected rendering: {line}"
    );
    assert_eq!(rendered.lines().count(), 1);
}

#[test]
fn custom_namespace_rejects_path_traversal() {
    assert!(Namespace::custom("roadmap").is_ok());
    for bad in ["", ".", "..", "../x", "a/b", "a\\b", "a:b"] {
        assert!(Namespace::custom(bad).is_err(), "{bad:?} must be rejected");
    }
    // Even a directly constructed `Custom` cannot make `namespace_dir`
    // or `concepts` escape the bundle root.
    let argosy = Argosy::open(fixture("valid-acme-billing")).unwrap();
    assert!(
        argosy
            .namespace_dir(&Namespace::Custom("..".into()))
            .is_none()
    );
    assert!(
        argosy
            .concepts(&Namespace::Custom("../../etc".into()))
            .unwrap()
            .is_empty()
    );
}

/// A symlinked namespace directory must not be entered: listing
/// pretends it does not exist rather than walking outside the bundle.
#[cfg(unix)]
#[test]
fn symlinked_namespace_is_not_entered() {
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(
        outside.path().join("secret.md"),
        "---\ntype: Secret\n---\nbody\n",
    )
    .unwrap();
    let bundle = tempfile::tempdir().unwrap();
    let root = bundle.path();
    std::fs::write(
        root.join("argosy.md"),
        "---\ntype: Argosy Manifest\nname: t\nargosy_version: \"1.0.0\"\n\
         okf_version: \"0.2\"\ndescription: t\n---\n# t\n",
    )
    .unwrap();
    std::os::unix::fs::symlink(outside.path(), root.join("skill")).unwrap();

    let argosy = Argosy::open(root).unwrap();
    assert!(argosy.namespace_dir(&Namespace::Skill).is_none());
    assert!(!argosy.namespaces_present().contains(&Namespace::Skill));
    assert!(argosy.concepts(&Namespace::Skill).unwrap().is_empty());
}

#[test]
fn namespace_names_round_trip() {
    for name in Namespace::RESERVED {
        let ns = Namespace::from_dir_name(name);
        assert!(ns.is_reserved());
        assert_eq!(ns.as_dir_name(), name);
    }
    let custom = Namespace::from_dir_name("roadmap");
    assert_eq!(custom, Namespace::Custom("roadmap".to_string()));
    assert!(!custom.is_reserved());
    assert_eq!(custom.as_dir_name(), "roadmap");
}

#[test]
fn manifest_parse_rejects_missing_name_and_version() {
    let concept = Concept::from_str("---\ntype: Argosy Manifest\n---\nbody\n").unwrap();
    assert!(Manifest::parse(&concept).is_err());
    let concept =
        Concept::from_str("---\ntype: Argosy Manifest\nname: x\nargosy_version: nope\n---\nbody\n")
            .unwrap();
    assert!(Manifest::parse(&concept).is_err());
}

/// A name outside the URI charset must fail at parse/open, not surface
/// later as `argosy://` URIs the resolver rejects.
#[test]
fn manifest_parse_rejects_unsafe_name_charset() {
    for bad in ["my bundle", "acme/billing", "ünïcode", ".."] {
        let concept = Concept::from_str(&format!(
            "---\ntype: Argosy Manifest\nname: {bad}\nargosy_version: \"1.0.0\"\n---\nbody\n"
        ))
        .unwrap();
        let err = Manifest::parse(&concept).unwrap_err();
        assert!(
            err.to_string().contains("URI charset"),
            "name `{bad}` should fail with a charset error, got: {err}"
        );
    }
    let concept = Concept::from_str(
        "---\ntype: Argosy Manifest\nname: acme-billing.v2\nargosy_version: \"1.0.0\"\n---\nbody\n",
    )
    .unwrap();
    assert!(Manifest::parse(&concept).is_ok());
}

/// `validate` and `open` must agree: a name outside the URI charset
/// fails to open (STR-5), so `validate` reports it as an error finding
/// instead of passing a bundle nothing else can use.
#[test]
fn validate_reports_unsafe_manifest_name_as_str5() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("argosy.md"),
        "---\ntype: Argosy Manifest\nname: my bundle\nargosy_version: \"1.0.0\"\n---\n# t\n",
    )
    .unwrap();
    let report = Argosy::validate(root);
    let str5: Vec<_> = report.errors().filter(|f| f.id == Some("STR-5")).collect();
    assert_eq!(str5.len(), 1, "one finding, got {report:?}");
    assert!(
        str5[0].message.contains("URI charset"),
        "unexpected message: {}",
        str5[0].message
    );
}

#[test]
fn open_reports_bad_manifest_fields_as_not_an_argosy() {
    let err = Argosy::open(fixture("bad-semver")).unwrap_err();
    assert!(
        matches!(err, Error::NotAnArgosy { .. }),
        "expected NotAnArgosy, got {err}"
    );
}

/// A permission-denied subdirectory must not abort validation: the rest
/// of the checks still run, and the finding names the offending path.
#[cfg(unix)]
#[test]
fn unreadable_subdirectory_is_a_targeted_str1_not_a_root_failure() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("argosy.md"),
        "---\ntype: Argosy Manifest\nname: t\nargosy_version: \"1.0.0\"\n\
         okf_version: \"0.2\"\ndescription: t\n---\n# t\n",
    )
    .unwrap();
    let secret = root.join("document/secret");
    std::fs::create_dir_all(&secret).unwrap();
    std::fs::write(secret.join("hidden.md"), "# hidden\n").unwrap();
    std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o000)).unwrap();

    let report = Argosy::validate(root);
    std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o755)).unwrap();

    let str1: Vec<_> = report.errors().filter(|f| f.id == Some("STR-1")).collect();
    assert_eq!(str1.len(), 1);
    assert_eq!(str1[0].path, Some(PathBuf::from("document/secret")));
    assert!(
        str1[0].message.contains("could not be read"),
        "unexpected message: {}",
        str1[0].message
    );
}
