//! Resource reads and listings.

use super::*;

#[test]
fn read_concept_resource_returns_markdown_with_identity_meta() {
    let mut rig = rig();
    let body = rig
        .state
        .read_resource("argosy://acme-billing/memory/gotchas")
        .unwrap();
    assert!(body.text.contains("type: Session Note"), "got {body:?}");
    assert!(body.text.contains("# Gotchas"));
    assert_eq!(body.mime, "text/markdown");
    let meta = body.meta.unwrap();
    assert_eq!(meta["argosy"], "acme-billing");
    assert_eq!(meta["namespace"], "memory");
    assert_eq!(meta["kind"], "local");
    assert_eq!(meta["trust"], "unverified");
}

#[test]
fn imported_concept_resources_arrive_delimiter_wrapped() {
    let mut rig = rig();
    let body = rig
        .state
        .read_resource("argosy://acme-shared/skill/shared-audit")
        .unwrap();
    assert_eq!(body.mime, "text/markdown");
    assert!(
        body.text.starts_with(
            "<untrusted-content argosy=\"acme-shared\" \
             uri=\"argosy://acme-shared/skill/shared-audit\" trust=\"unverified\">"
        ),
        "got {}",
        body.text
    );
    assert!(body.text.ends_with("</untrusted-content>"));
    let meta = body.meta.unwrap();
    assert_eq!(meta["kind"], "imported");
    assert_eq!(meta["trust"], "unverified");
}

#[test]
fn read_argosys_resource_lists_local_and_imported() {
    let mut rig = rig();
    let body = rig.state.read_resource(ARGOSYS_URI).unwrap();
    assert_eq!(body.mime, "application/json");
    let parsed: serde_json::Value = serde_json::from_str(&body.text).unwrap();
    let argosys = parsed["argosys"].as_array().unwrap();
    assert_eq!(argosys.len(), 2);
    assert_eq!(argosys[0]["name"], "acme-billing");
    assert_eq!(argosys[0]["kind"], "local");
    // Local concepts relay their own claims, so the local book gets no argosy-level tier.
    assert!(argosys[0].get("trust").is_none(), "got {parsed}");
    assert_eq!(argosys[1]["name"], "acme-shared");
    assert_eq!(argosys[1]["kind"], "imported");
    assert_eq!(argosys[1]["trust"], "unverified", "the configured cap");
}

#[test]
fn read_argosys_resource_reports_a_tampered_import_as_excluded() {
    // Contents tampered after the sidecar was written — the exclusion surfaces in the listing.
    let local = fixture_copy("valid-acme-billing");
    let imported = import_fixture();
    let packaged = tempfile::tempdir().unwrap();
    let book = packaged.path().join("book");
    crate::package::package(
        &crate::bundle::Argosy::open(imported.path()).unwrap(),
        &book,
        &crate::package::PackageOptions::default(),
    )
    .unwrap();
    fs::write(book.join("skill/shared-audit.md"), "Injected.\n").unwrap();
    let mut state = McpState::new(factory_with_trust(
        local.path().to_path_buf(),
        vec![book],
        crate::trust::TrustPolicy::new(),
    ));

    let body = state.read_resource(ARGOSYS_URI).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&body.text).unwrap();
    let argosys = parsed["argosys"].as_array().unwrap();
    assert_eq!(argosys.len(), 1, "only the local book serves, got {parsed}");
    let excluded = parsed["excluded"].as_array().unwrap();
    assert_eq!(excluded[0]["name"], "book");
    assert!(
        excluded[0]["reason"]
            .as_str()
            .unwrap()
            .contains("integrity"),
        "got {parsed}"
    );
}

#[test]
fn read_argosy_index_reads_the_root_index_md() {
    let mut rig = rig();
    let local_root = rig
        .state
        .session(project())
        .unwrap()
        .context
        .local()
        .root()
        .to_path_buf();
    fs::write(local_root.join("index.md"), "# Index\n\n- memory/gotchas\n").unwrap();

    let body = rig
        .state
        .read_resource("argosy://acme-billing/_index")
        .unwrap();
    assert!(body.text.contains("# Index"));

    rig.state
        .read_resource("argosy://acme-shared/_index")
        .unwrap_err();
    rig.state.read_resource("argosy://nope/_index").unwrap_err();
}

#[test]
fn read_resource_unknown_concept_and_argosy_error() {
    let mut rig = rig();
    let err = rig
        .state
        .read_resource("argosy://acme-billing/memory/nope")
        .unwrap_err();
    assert!(matches!(err, Error::ConceptNotFound { .. }), "got {err:?}");
    let err = rig
        .state
        .read_resource("argosy://nope/memory/gotchas")
        .unwrap_err();
    assert!(matches!(err, Error::UnknownArgosy { .. }), "got {err:?}");
    let err = rig.state.read_resource("not even a uri").unwrap_err();
    assert!(matches!(err, Error::InvalidUri { .. }), "got {err:?}");
}

#[test]
fn read_catalog_resource_renders_the_global_catalog_from_the_state_root() {
    let scratch = TempDir::new().unwrap();
    let state_root = scratch.path().join("argosy");
    let project_root = scratch.path().join("catalog-proj");
    fs::create_dir_all(&project_root).unwrap();
    let slot = crate::pull::record_project_root_at(&state_root, &project_root).unwrap();
    LocalArgosy::init(slot.join("default"), Some("catalog-proj"), None).unwrap();

    let mut rig = rig();
    rig.state = rig.state.with_catalog_state_root(&state_root);

    let body = rig.state.read_resource(CATALOG_URI).unwrap();
    assert_eq!(body.mime, "text/markdown");
    assert!(body.text.contains("# Argosy catalog"), "got {}", body.text);
    assert!(
        body.text.contains("catalog-proj (writable)"),
        "got {}",
        body.text
    );
    assert_eq!(body.uri, CATALOG_URI);

    let uris: Vec<String> = rig
        .state
        .list_resources()
        .unwrap()
        .into_iter()
        .map(|d| d.uri)
        .collect();
    assert!(uris.contains(&CATALOG_URI.to_string()), "got {uris:?}");
}

#[test]
fn read_catalog_resource_with_redaction_override_still_renders() {
    let scratch = TempDir::new().unwrap();
    let state_root = scratch.path().join("argosy");
    let project_root = scratch.path().join("redact-proj");
    fs::create_dir_all(&project_root).unwrap();
    let slot = crate::pull::record_project_root_at(&state_root, &project_root).unwrap();
    LocalArgosy::init(slot.join("default"), Some("redact-proj"), None).unwrap();

    let mut rig = rig();
    rig.state = rig
        .state
        .with_catalog_state_root(&state_root)
        .force_catalog_redaction();

    let body = rig.state.read_resource(CATALOG_URI).unwrap();
    assert!(
        body.text.contains("redact-proj (writable)"),
        "got {}",
        body.text
    );
}

#[test]
fn list_resources_advertises_argosys_and_present_indexes() {
    let mut rig = rig();
    let uris: Vec<String> = rig
        .state
        .list_resources()
        .unwrap()
        .into_iter()
        .map(|d| d.uri)
        .collect();
    assert!(uris.contains(&ARGOSYS_URI.to_string()));
    // Neither bundle has a root index.md yet.
    assert!(!uris.iter().any(|u| u.ends_with("/_index")), "got {uris:?}");

    let local_root = rig
        .state
        .session(project())
        .unwrap()
        .context
        .local()
        .root()
        .to_path_buf();
    fs::write(local_root.join("index.md"), "# Index\n").unwrap();
    let uris: Vec<String> = rig
        .state
        .list_resources()
        .unwrap()
        .into_iter()
        .map(|d| d.uri)
        .collect();
    assert!(uris.contains(&"argosy://acme-billing/_index".to_string()));
}
