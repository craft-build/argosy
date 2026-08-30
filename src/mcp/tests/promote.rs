//! The promote confirmation hook.

use super::*;

#[test]
fn promote_to_document_returns_source_and_draft_untouched_source() {
    let mut rig = rig();
    let before = rig
        .state
        .read_memory(ReadPathParams {
            cwd: project(),
            path: "memory/gotchas".to_string(),
        })
        .unwrap()
        .content;
    let out = rig
        .state
        .promote(PromoteParams {
            cwd: project(),
            source_path: "memory/gotchas".to_string(),
            target: PromoteTarget::Document,
            new_path: "document/processor-gotchas".to_string(),
            description: None,
        })
        .unwrap();
    assert_eq!(out.target, "document");
    assert_eq!(out.source_uri, "argosy://acme-billing/memory/gotchas");
    assert_eq!(out.source_content, before, "source content reported as-is");
    assert!(out.indexed, "promotion reconciles the index");
    assert_eq!(
        out.new_uri,
        "argosy://acme-billing/document/processor-gotchas"
    );
    let promoted = rig
        .state
        .read_resource("argosy://acme-billing/document/processor-gotchas")
        .unwrap();
    assert_eq!(promoted.text, out.drafted);
    // The memory file still exists.
    assert!(
        rig.state
            .session(project())
            .unwrap()
            .context
            .local()
            .root()
            .join("memory/gotchas.md")
            .is_file()
    );
}

#[test]
fn promote_to_styleguide_requires_a_description() {
    let mut rig = rig();
    let err = rig
        .state
        .promote(PromoteParams {
            cwd: project(),
            source_path: "memory/gotchas".to_string(),
            target: PromoteTarget::StyleguideRule,
            new_path: "styleguide/general/processor-gotchas".to_string(),
            description: None,
        })
        .unwrap_err();
    assert!(
        matches!(err, Error::NamespaceContractViolation { .. }),
        "got {err:?}"
    );

    let out = rig
        .state
        .promote(PromoteParams {
            cwd: project(),
            source_path: "memory/gotchas".to_string(),
            target: PromoteTarget::StyleguideRule,
            new_path: "styleguide/general/processor-gotchas".to_string(),
            description: Some("Retry accounting uses the original timestamp.".to_string()),
        })
        .unwrap();
    assert_eq!(out.target, "styleguide");
    assert!(out.drafted.contains("type: Styleguide Rule"));
    assert!(out.drafted.contains("original timestamp"));
    assert!(out.indexed);
}
