//! Unit tests for the local argosy's write surface.

use std::fs;
use std::str::FromStr;

use tempfile::TempDir;

use super::*;
use crate::bundle::Severity;
use crate::error::Error;
use crate::testutil::fixture_copy;

fn id(s: &str) -> ConceptId {
    ConceptId::from_str(s).unwrap()
}

fn note_concept() -> Concept {
    Concept::from_str("---\ntype: Session Note\n---\n# Note\n\nContent.\n").unwrap()
}

// --- init ---

#[test]
fn init_creates_a_conformant_bundle_and_derives_the_name() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("my-bundle");

    let local = LocalArgosy::init(&root, None, Some("What this bundle knows.")).unwrap();

    assert_eq!(local.manifest().name(), "my-bundle");
    assert_eq!(local.manifest().argosy_version().to_string(), "0.1.0");
    for namespace in Namespace::RESERVED {
        assert!(root.join(namespace).is_dir(), "missing {namespace}/");
    }
    assert!(root.join("argosy.md").is_file());
    let report = Argosy::validate(&root);
    assert!(
        report.is_conformant(),
        "a freshly initialized bundle opens and validates: {report}"
    );
}

#[test]
fn init_second_run_fails_and_changes_nothing() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("bundle");
    LocalArgosy::init(&root, None, None).unwrap();
    let manifest_before = fs::read_to_string(root.join("argosy.md")).unwrap();

    let err = LocalArgosy::init(&root, Some("other"), None).unwrap_err();
    assert!(
        err.to_string().contains("already contains an argosy"),
        "unexpected error: {err}"
    );
    assert_eq!(
        fs::read_to_string(root.join("argosy.md")).unwrap(),
        manifest_before
    );
}

#[test]
fn init_rejects_names_outside_the_uri_charset() {
    let tmp = TempDir::new().unwrap();
    for bad in [
        "has spaces",
        "has/slash",
        "ünicode",
        "..",
        "",
        "trailing:colon",
    ] {
        let err = LocalArgosy::init(tmp.path().join("fresh"), Some(bad), None).unwrap_err();
        assert!(
            err.to_string().contains("invalid bundle name"),
            "`{bad}`: unexpected error: {err}"
        );
    }
}

#[test]
fn init_honors_an_explicit_name_over_the_directory_basename() {
    let tmp = TempDir::new().unwrap();
    let local = LocalArgosy::init(tmp.path().join("wrong-name"), Some("right-name"), None).unwrap();
    assert_eq!(local.manifest().name(), "right-name");
}

fn rule_concept() -> Concept {
    Concept::from_str(
        "---\ntype: Styleguide Rule\ndescription: Never swallow errors.\n---\n# Rule\n\nHandle them.\n",
    )
    .unwrap()
}

#[test]
fn write_memory_round_trips_and_creates_nested_dirs() {
    let tmp = fixture_copy("valid-acme-billing");
    let local = LocalArgosy::open(tmp.path()).unwrap();
    let concept = note_concept();
    let path = local
        .write_memory(&id("memory/sessions/2026-08/rate-limit"), &concept)
        .unwrap();
    assert!(path.ends_with("memory/sessions/2026-08/rate-limit.md"));
    let reread = Concept::from_file(&path).unwrap();
    assert_eq!(reread, concept);
    assert!(
        local
            .concepts(&Namespace::Memory)
            .unwrap()
            .iter()
            .any(|(cid, _)| cid.as_str() == "memory/sessions/2026-08/rate-limit")
    );
}

#[test]
fn write_rule_round_trips_in_nested_subdirs() {
    let tmp = fixture_copy("valid-acme-billing");
    let local = LocalArgosy::open(tmp.path()).unwrap();
    let concept = rule_concept();
    let path = local
        .write_rule(
            &id("styleguide/python/error-handling/no-bare-except"),
            &concept,
        )
        .unwrap();
    assert_eq!(Concept::from_file(&path).unwrap(), concept);
    let rules = crate::styleguide::StyleguideRule::list(&local).unwrap();
    assert!(
        rules
            .iter()
            .any(|r| r.id().as_str() == "styleguide/python/error-handling/no-bare-except")
    );
}

#[test]
fn write_document_round_trips() {
    let tmp = fixture_copy("valid-acme-billing");
    let local = LocalArgosy::open(tmp.path()).unwrap();
    let concept = Concept::from_str(
        "---\ntype: Decision\ndescription: Cache responses.\n---\n# Decision\n\nWe cache.\n",
    )
    .unwrap();
    let path = local
        .write_document(&id("document/decisions/2026-08-caching"), &concept)
        .unwrap();
    assert_eq!(Concept::from_file(&path).unwrap(), concept);
}

#[test]
fn delete_document_removes_and_prunes_parents() {
    let tmp = fixture_copy("valid-acme-billing");
    let local = LocalArgosy::open(tmp.path()).unwrap();
    local
        .write_document(
            &id("document/scratch/2026-08-caching"),
            &Concept::from_str("---\ntype: Decision\n---\n# D\n\nBody.\n").unwrap(),
        )
        .unwrap();

    local
        .delete_document(&id("document/scratch/2026-08-caching"))
        .unwrap();
    assert!(!tmp.path().join("document/scratch").exists());
    assert!(
        tmp.path().join("document/architecture.md").is_file(),
        "sibling documents survive"
    );
    // The pre-existing decisions/ tree is untouched.
    assert!(
        tmp.path()
            .join("document/decisions/2026-05-caching.md")
            .is_file()
    );

    let err = local
        .delete_document(&id("document/scratch/2026-08-caching"))
        .unwrap_err();
    assert!(matches!(err, Error::ConceptNotFound { .. }), "got {err:?}");
}

#[test]
fn write_refuses_reserved_filename() {
    let tmp = fixture_copy("valid-acme-billing");
    let local = LocalArgosy::open(tmp.path()).unwrap();
    for target in ["memory/index", "memory/log", "memory/argosy"] {
        let err = local
            .write_memory(&id(target), &note_concept())
            .unwrap_err();
        assert!(
            matches!(err, Error::ReservedFilename),
            "{target}: got {err:?}"
        );
        assert!(!tmp.path().join(format!("{target}.md")).exists());
    }
}

#[test]
fn write_refuses_custom_namespace() {
    let tmp = fixture_copy("valid-acme-billing");
    let local = LocalArgosy::open(tmp.path()).unwrap();
    let err = local
        .write_concept(
            Namespace::custom("roadmap").unwrap(),
            &id("roadmap/plans-2"),
            &note_concept(),
        )
        .unwrap_err();
    assert!(matches!(err, Error::Validation { .. }), "got {err:?}");
    assert!(err.to_string().contains("producer-owned"));
}

#[test]
fn write_memory_auto_fills_missing_type() {
    let tmp = fixture_copy("valid-acme-billing");
    let local = LocalArgosy::open(tmp.path()).unwrap();
    // No frontmatter at all, and an empty `type` alongside a real field:
    // both get `type: Memory` rather than a MEM-1 rejection.
    local
        .write_memory(
            &id("memory/x"),
            &Concept::from_str("# Just prose\n").unwrap(),
        )
        .unwrap();
    local
        .write_memory(
            &id("memory/y"),
            &Concept::from_str("---\ntype: \"\"\ndescription: d\n---\n# Y\n").unwrap(),
        )
        .unwrap();
    for target in ["memory/x", "memory/y"] {
        let written = Concept::from_file(tmp.path().join(format!("{target}.md"))).unwrap();
        assert_eq!(written.concept_type(), Some("Memory"), "{target}");
    }
    // What landed on disk satisfies MEM-1: the bundle still validates.
    let report = Argosy::validate(tmp.path());
    assert!(report.is_conformant(), "{report:?}");
}

#[test]
fn write_document_still_refuses_untyped_concept() {
    let tmp = fixture_copy("valid-acme-billing");
    let local = LocalArgosy::open(tmp.path()).unwrap();
    let untyped = Concept::from_str("# Just prose\n").unwrap();
    let err = local
        .write_document(&id("document/x"), &untyped)
        .unwrap_err();
    match err {
        Error::NamespaceContractViolation { requirement, .. } => {
            assert_eq!(requirement, "DOC-1")
        }
        other => panic!("got {other:?}"),
    }
}

#[test]
fn write_rule_without_rule_type_is_stg2() {
    let tmp = fixture_copy("valid-acme-billing");
    let local = LocalArgosy::open(tmp.path()).unwrap();
    let err = local
        .write_rule(&id("styleguide/rust/naming/no-rule-type"), &note_concept())
        .unwrap_err();
    match err {
        Error::NamespaceContractViolation { requirement, .. } => {
            assert_eq!(requirement, "STG-2")
        }
        other => panic!("got {other:?}"),
    }
}

#[test]
fn write_rule_without_description_is_stg3() {
    let tmp = fixture_copy("valid-acme-billing");
    let local = LocalArgosy::open(tmp.path()).unwrap();
    let concept = Concept::from_str("---\ntype: Styleguide Rule\n---\n# Rule\n\nBody.\n").unwrap();
    let err = local
        .write_rule(&id("styleguide/rust/naming/no-desc"), &concept)
        .unwrap_err();
    match err {
        Error::NamespaceContractViolation { requirement, .. } => {
            assert_eq!(requirement, "STG-3")
        }
        other => panic!("got {other:?}"),
    }
}

#[test]
fn write_skill_entry_point_without_description_is_skl4() {
    let tmp = fixture_copy("valid-acme-billing");
    let local = LocalArgosy::open(tmp.path()).unwrap();
    let concept = Concept::from_str("---\ntype: Skill\n---\n# Deploy\n\nSteps.\n").unwrap();
    let err = local
        .write_concept(Namespace::Skill, &id("skill/deploy-new"), &concept)
        .unwrap_err();
    match err {
        Error::NamespaceContractViolation { requirement, .. } => {
            assert_eq!(requirement, "SKL-4")
        }
        other => panic!("got {other:?}"),
    }
}

#[test]
fn write_skill_supporting_material_needs_only_okf_conformance() {
    let tmp = fixture_copy("valid-acme-billing");
    let local = LocalArgosy::open(tmp.path()).unwrap();
    // Not an entry-point position (a `references/` material): a typed
    // concept with no description and the "wrong" type writes fine.
    let concept = Concept::from_str("---\ntype: Note\n---\n# Extra\n\nMaterial.\n").unwrap();
    local
        .write_concept(
            Namespace::Skill,
            &id("skill/rotate-api-keys/references/extra"),
            &concept,
        )
        .unwrap();
}

#[test]
fn concept_id_rejects_dotdot() {
    let err = ConceptId::from_str("memory/../secret").unwrap_err();
    assert!(err.to_string().contains(".."), "got {err}");
    let err = Namespace::custom("..").unwrap_err();
    assert!(err.to_string().contains("invalid"), "got {err}");
}

#[test]
fn write_refuses_id_outside_the_named_namespace() {
    let tmp = fixture_copy("valid-acme-billing");
    let local = LocalArgosy::open(tmp.path()).unwrap();
    let err = local
        .write_memory(&id("document/escaped"), &note_concept())
        .unwrap_err();
    assert!(matches!(err, Error::Validation { .. }), "got {err:?}");
}

#[test]
fn promote_to_document_copies_and_cites_source() {
    let tmp = fixture_copy("valid-acme-billing");
    let local = LocalArgosy::open(tmp.path()).unwrap();
    let source_path = tmp.path().join("memory/gotchas.md");
    let before = fs::read(&source_path).unwrap();

    let promotion = local
        .promote_memory(
            &id("memory/gotchas"),
            PromotionTarget::Document,
            &id("document/rate-limit-retry-gotcha"),
            None,
        )
        .unwrap();

    // The source is byte-identical afterwards.
    assert_eq!(fs::read(&source_path).unwrap(), before);
    // A new, independent concept exists at the target id.
    let drafted = &promotion.drafted;
    let listed = local
        .concepts(&Namespace::Document)
        .unwrap()
        .into_iter()
        .find(|(cid, _)| cid.as_str() == "document/rate-limit-retry-gotcha")
        .unwrap()
        .1;
    assert_eq!(listed, *drafted, "what was written is what was returned");
    assert_eq!(drafted.concept_type(), Some("Session Note"));
    // `sources` cites the bundle-relative memory path.
    let sources = drafted.get("sources").unwrap().as_sequence().unwrap();
    assert_eq!(sources.len(), 1);
    assert_eq!(
        sources[0].get("resource").unwrap().as_str().unwrap(),
        "memory/gotchas.md"
    );

    // No silent overwrites: promoting again to the same id fails.
    let err = local
        .promote_memory(
            &id("memory/gotchas"),
            PromotionTarget::Document,
            &id("document/rate-limit-retry-gotcha"),
            None,
        )
        .unwrap_err();
    assert!(matches!(err, Error::ConceptExists { .. }), "got {err:?}");
}

#[test]
fn promote_to_styleguide_requires_a_description() {
    let tmp = fixture_copy("valid-acme-billing");
    let local = LocalArgosy::open(tmp.path()).unwrap();
    // `memory/gotchas.md` has no `description`; without an override the
    // promotion must fail rather than write an invalid rule.
    let err = local
        .promote_memory(
            &id("memory/gotchas"),
            PromotionTarget::StyleguideRule,
            &id("styleguide/general/rate-limit-retry"),
            None,
        )
        .unwrap_err();
    match err {
        Error::NamespaceContractViolation { requirement, .. } => {
            assert_eq!(requirement, "STG-3")
        }
        other => panic!("got {other:?}"),
    }
    assert!(!tmp.path().join("styleguide/general").exists());
}

#[test]
fn promote_to_styleguide_uses_override_and_sets_rule_type() {
    let tmp = fixture_copy("valid-acme-billing");
    let local = LocalArgosy::open(tmp.path()).unwrap();
    let promotion = local
        .promote_memory(
            &id("memory/gotchas"),
            PromotionTarget::StyleguideRule,
            &id("styleguide/general/rate-limit-retry"),
            Some("Preserve the original timestamp when retrying."),
        )
        .unwrap();
    assert_eq!(
        promotion.drafted.concept_type(),
        Some("Styleguide Rule"),
        "PROM-4/STG-2"
    );
    assert_eq!(
        promotion.drafted.description(),
        Some("Preserve the original timestamp when retrying.")
    );
    let sources = promotion
        .drafted
        .get("sources")
        .unwrap()
        .as_sequence()
        .unwrap();
    assert_eq!(
        sources[0].get("resource").unwrap().as_str().unwrap(),
        "memory/gotchas.md"
    );
    // The written rule is listable like any other.
    let rules = crate::styleguide::StyleguideRule::list(&local).unwrap();
    assert!(
        rules
            .iter()
            .any(|r| r.id().as_str() == "styleguide/general/rate-limit-retry")
    );
}

#[test]
fn promote_preserves_preseeded_sources() {
    let tmp = fixture_copy("valid-acme-billing");
    let local = LocalArgosy::open(tmp.path()).unwrap();
    let concept = Concept::from_str(
        "---\ntype: Session Note\ndescription: d\nsources:\n  - resource: document/architecture.md\n---\nBody.\n",
    )
    .unwrap();
    local
        .write_memory(&id("memory/with-sources"), &concept)
        .unwrap();
    let promotion = local
        .promote_memory(
            &id("memory/with-sources"),
            PromotionTarget::Document,
            &id("document/promoted-with-sources"),
            None,
        )
        .unwrap();
    let resources: Vec<_> = promotion
        .drafted
        .get("sources")
        .unwrap()
        .as_sequence()
        .unwrap()
        .iter()
        .map(|s| s.get("resource").unwrap().as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        resources,
        vec!["document/architecture.md", "memory/with-sources.md"]
    );
}

/// Provenance is append, never replace: a hand-edited scalar `sources`
/// must error, not silently become a fresh one-element list.
#[test]
fn promote_refuses_non_list_sources_instead_of_replacing() {
    let tmp = fixture_copy("valid-acme-billing");
    let local = LocalArgosy::open(tmp.path()).unwrap();
    let concept = Concept::from_str(
        "---\ntype: Session Note\ndescription: d\nsources: jira-123\n---\nBody.\n",
    )
    .unwrap();
    local
        .write_memory(&id("memory/bad-sources"), &concept)
        .unwrap();

    let err = local
        .promote_memory(
            &id("memory/bad-sources"),
            PromotionTarget::Document,
            &id("document/promoted-bad-sources"),
            None,
        )
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("`sources` frontmatter is not a list"),
        "{err}"
    );
    assert!(!tmp.path().join("document/promoted-bad-sources.md").exists());
}

/// The refusal names the concrete directory to remove out-of-band, since
/// no argosy API deletes skill directories.
#[test]
fn deleting_a_directory_skill_entry_point_names_the_directory() {
    let tmp = fixture_copy("valid-acme-billing");
    let local = LocalArgosy::open(tmp.path()).unwrap();
    let err = local
        .delete_concept(
            Namespace::Skill,
            &id("skill/rotate-api-keys/rotate-api-keys"),
        )
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("no argosy API deletes skill directories"),
        "{msg}"
    );
    assert!(
        msg.contains("rotate-api-keys"),
        "must name the skill directory: {msg}"
    );
}

#[test]
fn promote_leaves_source_and_tree_conformant() {
    let tmp = fixture_copy("valid-acme-billing");
    let local = LocalArgosy::open(tmp.path()).unwrap();
    local
        .promote_memory(
            &id("memory/gotchas"),
            PromotionTarget::Document,
            &id("document/rate-limit-retry-gotcha"),
            None,
        )
        .unwrap();
    // The source stays in memory/ (auto-delete never happens).
    assert!(tmp.path().join("memory/gotchas.md").is_file());
    assert!(
        local
            .concepts(&Namespace::Memory)
            .unwrap()
            .iter()
            .any(|(cid, _)| cid.as_str() == "memory/gotchas")
    );
    // The whole tree (with the promoted concept) still validates clean.
    let report = Argosy::validate(tmp.path());
    assert!(
        report.errors().next().is_none(),
        "unexpected errors:\n{report}"
    );
    // Info about memory/ is expected; nothing worse.
    assert!(
        report
            .findings()
            .iter()
            .all(|f| f.severity != Severity::Warning
                || f.id == Some("§4.2")
                || f.id == Some("STG-4"))
    );
}

#[test]
fn promote_unknown_source_is_not_found() {
    let tmp = fixture_copy("valid-acme-billing");
    let local = LocalArgosy::open(tmp.path()).unwrap();
    let err = local
        .promote_memory(
            &id("memory/nope"),
            PromotionTarget::Document,
            &id("document/x"),
            None,
        )
        .unwrap_err();
    assert!(matches!(err, Error::ConceptNotFound { .. }), "got {err:?}");
}

#[test]
fn promote_refuses_non_memory_source() {
    let tmp = fixture_copy("valid-acme-billing");
    let local = LocalArgosy::open(tmp.path()).unwrap();
    let err = local
        .promote_memory(
            &id("document/architecture"),
            PromotionTarget::Document,
            &id("document/x"),
            None,
        )
        .unwrap_err();
    assert!(matches!(err, Error::Validation { .. }), "got {err:?}");
}

#[test]
fn delete_memory_prunes_empty_parents_to_namespace_root() {
    let tmp = fixture_copy("valid-acme-billing");
    let local = LocalArgosy::open(tmp.path()).unwrap();
    local
        .write_memory(&id("memory/tmp/scratch/note"), &note_concept())
        .unwrap();
    local.delete_memory(&id("memory/tmp/scratch/note")).unwrap();
    assert!(!tmp.path().join("memory/tmp").exists());
    assert!(
        tmp.path().join("memory").is_dir(),
        "namespace root survives"
    );
    assert!(tmp.path().join("memory/gotchas.md").is_file());
}

#[test]
fn delete_missing_concept_is_not_found() {
    let tmp = fixture_copy("valid-acme-billing");
    let local = LocalArgosy::open(tmp.path()).unwrap();
    let err = local.delete_memory(&id("memory/nope")).unwrap_err();
    assert!(matches!(err, Error::ConceptNotFound { .. }), "got {err:?}");
}

#[test]
fn delete_refuses_directory_form_skill_entry_point() {
    let tmp = fixture_copy("valid-acme-billing");
    let local = LocalArgosy::open(tmp.path()).unwrap();
    let err = local
        .delete_concept(
            Namespace::Skill,
            &id("skill/rotate-api-keys/rotate-api-keys"),
        )
        .unwrap_err();
    match err {
        Error::NamespaceContractViolation {
            requirement,
            detail,
        } => {
            assert_eq!(requirement, "SKL-2");
            assert!(detail.contains("skill/rotate-api-keys/"), "got {detail}");
        }
        other => panic!("got {other:?}"),
    }
    assert!(
        tmp.path()
            .join("skill/rotate-api-keys/rotate-api-keys.md")
            .is_file()
    );
}

#[test]
fn delete_file_form_skill_entry_point_works() {
    let tmp = fixture_copy("valid-acme-billing");
    let local = LocalArgosy::open(tmp.path()).unwrap();
    local
        .delete_concept(Namespace::Skill, &id("skill/reconcile-ledger"))
        .unwrap();
    assert!(!tmp.path().join("skill/reconcile-ledger.md").exists());
    assert!(tmp.path().join("skill").is_dir());
}
