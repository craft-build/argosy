//! Search tools: `search`, `search_rules`, and `read`.

use super::*;

#[test]
fn search_k_defaults_to_eight_and_every_filter_field_maps() {
    let mut rig = rig();
    // The index holds 8 units (3 documents + 2 skills + 1 memory + 1 rule
    // local, 1 skill imported); a ninth makes the default observable.
    let ninth: Concept = ("---\n\
         type: Reference\n\
         description: Settlement report layout.\n\
         tags:\n  - e2e-tag\n\
         ---\n\
         # Layout\n\nColumns.\n")
        .parse()
        .unwrap();
    rig.state
        .session(project())
        .unwrap()
        .context
        .local()
        .write_concept(
            Namespace::Document,
            &"document/settlement-layout".parse().unwrap(),
            &ninth,
        )
        .unwrap();
    // A fresh state over the same (now nine-unit) roots, so its factory
    // reconciles a fresh index over the new unit count.
    let mut state = McpState::new(factory(
        rig.state
            .session(project())
            .unwrap()
            .context
            .local()
            .root()
            .to_path_buf(),
        rig.state
            .session(project())
            .unwrap()
            .context
            .imported()
            .map(|a| a.root().to_path_buf())
            .collect(),
    ));

    let broad = |cwd: PathBuf| SearchParams {
        cwd,
        query: "billing ledger settlement processor".to_string(),
        k: None,
        namespaces: None,
        argosy: None,
        tags: None,
        r#type: None,
        language: None,
        category: None,
    };
    let default_k = state.search(broad(project())).unwrap();
    assert_eq!(
        default_k.hits.len(),
        8,
        "k defaults to 8, truncating 10 units"
    );
    let wide = state
        .search(SearchParams {
            k: Some(20),
            ..broad(project())
        })
        .unwrap();
    assert_eq!(wide.hits.len(), 10, "explicit k lifts the truncation");

    let tagged = state
        .search(SearchParams {
            tags: Some(vec!["e2e-tag".to_string()]),
            ..broad(project())
        })
        .unwrap();
    assert_eq!(
        tagged
            .hits
            .iter()
            .map(|h| h.uri.as_str())
            .collect::<Vec<_>>(),
        ["argosy://acme-billing/document/settlement-layout"],
        "`tags` maps to Filter::tags"
    );

    let typed = state
        .search(SearchParams {
            r#type: Some("Session Note".to_string()),
            ..broad(project())
        })
        .unwrap();
    assert_eq!(
        typed
            .hits
            .iter()
            .map(|h| h.uri.as_str())
            .collect::<Vec<_>>(),
        ["argosy://acme-billing/memory/gotchas"],
        "`type` maps to Filter::concept_types"
    );

    let scoped_ns = state
        .search(SearchParams {
            namespaces: Some(vec!["document".to_string()]),
            ..broad(project())
        })
        .unwrap();
    assert!(!scoped_ns.hits.is_empty());
    assert!(
        scoped_ns.hits.iter().all(|h| h.namespace == "document"),
        "`namespaces` maps to Filter::namespaces"
    );
}

#[test]
fn search_returns_qualified_hits() {
    let mut rig = rig();
    let report = rig
        .state
        .search(SearchParams {
            cwd: project(),
            query: "rate limit retries original request timestamp".to_string(),
            k: None,
            namespaces: None,
            argosy: None,
            tags: None,
            r#type: None,
            language: None,
            category: None,
        })
        .unwrap();
    assert!(!report.hits.is_empty());
    let top = &report.hits[0];
    assert!(
        top.uri.starts_with("argosy://"),
        "qualified uri, got {}",
        top.uri
    );
    assert!(
        report
            .hits
            .iter()
            .any(|h| h.uri == "argosy://acme-billing/document/rate-limit-behavior"),
        "the semantic match for the rate-limit note appears"
    );

    let scoped = rig
        .state
        .search(SearchParams {
            cwd: project(),
            query: "rate limit".to_string(),
            k: None,
            namespaces: None,
            argosy: Some("acme-shared".to_string()),
            tags: None,
            r#type: None,
            language: None,
            category: None,
        })
        .unwrap();
    assert!(
        scoped.hits.iter().all(|h| h.argosy == "acme-shared"),
        "scope honored"
    );
}

#[test]
fn search_with_inactive_argosy_name_errors() {
    let mut rig = rig();
    let err = rig
        .state
        .search(SearchParams {
            cwd: project(),
            query: "anything".to_string(),
            k: None,
            namespaces: None,
            argosy: Some("not-active".to_string()),
            tags: None,
            r#type: None,
            language: None,
            category: None,
        })
        .unwrap_err();
    assert!(matches!(err, Error::UnknownArgosy { .. }), "got {err:?}");
}

#[test]
fn search_with_unknown_namespace_errors() {
    let mut rig = rig();
    let err = rig
        .state
        .search(SearchParams {
            cwd: project(),
            query: "anything".to_string(),
            k: None,
            namespaces: Some(vec!["documnet".to_string()]),
            argosy: None,
            tags: None,
            r#type: None,
            language: None,
            category: None,
        })
        .unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains("unknown namespace `documnet`"),
        "names the typo, got: {message}"
    );
    assert!(
        message.contains("styleguide"),
        "lists the valid namespaces, got: {message}"
    );
}

#[test]
fn search_rules_hits_carry_good_and_bad_sections() {
    let mut rig = rig();
    let report = rig
        .state
        .search_rules(RulesParams {
            cwd: project(),
            query: "variable naming conventions".to_string(),
            language: None,
            category: None,
            k: None,
        })
        .unwrap();
    let hit = report
        .hits
        .iter()
        .find(|h| h.concept_id.ends_with("snake-case-vars"))
        .expect("fixture rule is a hit");
    assert_eq!(hit.good.as_deref(), Some("let retry_count = 0;"));
    assert_eq!(hit.bad.as_deref(), Some("let retryCount = 0;"));

    // Plain `search` over the same rule does not enrich — the examples
    // are the review-flow (`search_rules`) contract.
    let plain = rig
        .state
        .search(SearchParams {
            cwd: project(),
            query: "variable naming conventions".to_string(),
            k: None,
            namespaces: Some(vec!["styleguide".to_string()]),
            argosy: None,
            tags: None,
            r#type: None,
            language: None,
            category: None,
        })
        .unwrap();
    let hit = plain
        .hits
        .iter()
        .find(|h| h.concept_id.ends_with("snake-case-vars"))
        .expect("fixture rule is a hit");
    assert!(hit.good.is_none() && hit.bad.is_none());
}

#[test]
fn read_defaults_to_local_and_reads_imported_by_name() {
    let mut rig = rig();
    let local = rig
        .state
        .read(ReadParams {
            cwd: project(),
            path: "memory/gotchas".to_string(),
            argosy: None,
        })
        .unwrap();
    assert_eq!(local.argosy, "acme-billing");
    assert_eq!(local.kind, "local");
    assert_eq!(local.uri, "argosy://acme-billing/memory/gotchas");
    assert!(local.content.contains("# Gotchas"));

    // The gap this tool closes: reading an imported argosy's concept
    // through the tool surface (resources can only serve the process
    // working directory's project).
    let imported = rig
        .state
        .read(ReadParams {
            cwd: project(),
            path: "skill/shared-audit".to_string(),
            argosy: Some("acme-shared".to_string()),
        })
        .unwrap();
    assert_eq!(imported.argosy, "acme-shared");
    assert_eq!(imported.kind, "imported");
    assert_eq!(imported.uri, "argosy://acme-shared/skill/shared-audit");
    assert!(imported.content.contains("Steps."));
}

#[test]
fn read_with_unknown_argosy_errors() {
    let mut rig = rig();
    let err = rig
        .state
        .read(ReadParams {
            cwd: project(),
            path: "memory/gotchas".to_string(),
            argosy: Some("not-active".to_string()),
        })
        .unwrap_err();
    assert!(matches!(err, Error::UnknownArgosy { .. }), "got {err:?}");
}

#[test]
fn search_rules_hits_only_styleguide_and_facets_apply() {
    let mut rig = rig();
    let report = rig
        .state
        .search_rules(RulesParams {
            cwd: project(),
            query: "variable naming conventions".to_string(),
            language: None,
            category: None,
            k: None,
        })
        .unwrap();
    assert!(!report.hits.is_empty());
    assert!(
        report.hits.iter().all(|h| h.namespace == "styleguide"),
        "all hits are rules"
    );
    assert_eq!(report.hits[0].language.as_deref(), Some("rust"));
    assert_eq!(report.hits[0].category.as_deref(), Some("naming"));

    let none = rig
        .state
        .search_rules(RulesParams {
            cwd: project(),
            query: "variable naming".to_string(),
            language: Some("python".to_string()),
            category: None,
            k: None,
        })
        .unwrap();
    assert!(none.hits.is_empty(), "facet mismatch excludes");
}

// --- Provenance tagging of hits and reads (SEC-1/SEC-2) ---

#[test]
fn hits_carry_kind_trust_and_source_commit() {
    // Without configured trust, an import's `verified: machine-confirmed`
    // self-claim must not relay: the hit reads kind=imported,
    // trust=unverified.
    let mut rig = rig();
    let report = rig
        .state
        .search(SearchParams {
            cwd: project(),
            query: "audit the shared provisioner".to_string(),
            k: None,
            namespaces: None,
            argosy: None,
            tags: None,
            r#type: None,
            language: None,
            category: None,
        })
        .unwrap();
    let imported = report
        .hits
        .iter()
        .find(|h| h.argosy == "acme-shared")
        .expect("imported skill is a hit");
    assert_eq!(imported.kind, "imported");
    assert_eq!(imported.trust, "unverified", "no configured cap");
    assert_eq!(
        imported.source_commit, None,
        "the plain-dir fixture has no git metadata"
    );

    let local = report
        .hits
        .iter()
        .find(|h| h.argosy == "acme-billing")
        .expect("local concept is a hit");
    assert_eq!(local.kind, "local");
    assert_eq!(local.source_commit, None);
}

#[test]
fn imported_hit_trust_is_capped_not_claimed() {
    // The same fixture under a configured cap of machine-confirmed: the
    // import's claim (machine-confirmed) stands at the cap, but a
    // human-reviewed claim would read machine-confirmed — the cap is a
    // ceiling, not a grant.
    let mut rig = rig_with_trust(crate::trust::TrustPolicy::from_caps(
        [(
            "acme-shared".to_string(),
            crate::trust::TrustTier::MachineConfirmed,
        )]
        .into_iter()
        .collect(),
    ));
    let report = rig
        .state
        .search(SearchParams {
            cwd: project(),
            query: "audit the shared provisioner".to_string(),
            k: None,
            namespaces: None,
            argosy: None,
            tags: None,
            r#type: None,
            language: None,
            category: None,
        })
        .unwrap();
    let imported = report
        .hits
        .iter()
        .find(|h| h.argosy == "acme-shared")
        .expect("imported skill is a hit");
    assert_eq!(imported.trust, "machine-confirmed");
}

#[test]
fn local_hits_relay_their_own_claim_verbatim() {
    let mut rig = rig();
    let claimed: Concept = ("---\n\
         type: Memory\n\
         description: Pinned facts about the billing ledger.\n\
         verified: human-reviewed\n\
         ---\n\
         # Pinned\n\nLedger facts.\n")
        .parse()
        .unwrap();
    rig.state
        .session(project())
        .unwrap()
        .context
        .local()
        .write_concept(
            Namespace::Memory,
            &"memory/pinned-facts".parse().unwrap(),
            &claimed,
        )
        .unwrap();
    // A fresh state over the same (now larger) local root, so its factory
    // reconciles an index that includes the written concept.
    let mut state = McpState::new(factory(
        rig.state
            .session(project())
            .unwrap()
            .context
            .local()
            .root()
            .to_path_buf(),
        Vec::new(),
    ));
    let report = state
        .search(SearchParams {
            cwd: project(),
            query: "pinned facts billing ledger".to_string(),
            k: None,
            namespaces: None,
            argosy: None,
            tags: None,
            r#type: None,
            language: None,
            category: None,
        })
        .unwrap();
    let hit = report
        .hits
        .iter()
        .find(|h| h.uri.ends_with("memory/pinned-facts"))
        .expect("written memory is a hit");
    assert_eq!(hit.kind, "local");
    assert_eq!(
        hit.trust, "human-reviewed",
        "the local book grades its own writing"
    );
}

#[test]
fn imported_reads_arrive_delimiter_wrapped() {
    let mut rig = rig();
    let imported = rig
        .state
        .read(ReadParams {
            cwd: project(),
            path: "skill/shared-audit".to_string(),
            argosy: Some("acme-shared".to_string()),
        })
        .unwrap();
    assert_eq!(imported.kind, "imported");
    assert_eq!(imported.trust, "unverified");
    assert_eq!(imported.source_commit, None);
    assert!(
        imported.content.starts_with(
            "<untrusted-content argosy=\"acme-shared\" \
             uri=\"argosy://acme-shared/skill/shared-audit\" trust=\"unverified\">"
        ),
        "opened by the delimiter: {}",
        imported.content
    );
    assert!(imported.content.ends_with("</untrusted-content>"));
    assert!(imported.content.contains("# Audit"));

    // The local book is the user's own writing: never wrapped.
    let local = rig
        .state
        .read(ReadParams {
            cwd: project(),
            path: "memory/gotchas".to_string(),
            argosy: None,
        })
        .unwrap();
    assert_eq!(local.kind, "local");
    assert_eq!(local.trust, "unverified", "absent claim reads as the floor");
    assert!(
        !local.content.contains("untrusted-content"),
        "local reads stay bare: {}",
        local.content
    );
}

#[test]
fn search_rules_wraps_imported_examples_in_untrusted_delimiters() {
    // A bespoke import carrying a rule with Good/Bad sections: the review
    // flow hands those excerpts to the reviewer prompt, so they get the
    // same delimiters full bodies do.
    let imported = tempfile::tempdir().unwrap();
    let book = LocalArgosy::init(imported.path(), Some("acme-shared"), None).unwrap();
    let rule: Concept = ("---\n\
         type: Styleguide Rule\n\
         description: Name retry counters plainly in snake case.\n\
         language: rust\n\
         category: naming\n\
         verified: human-reviewed\n\
         ---\n\
         # shared-rule\n\n\
         Use snake case.\n\n\
         ## Good\n\n\
         let retry_count = 0;\n\n\
         ## Bad\n\n\
         let retryCount = 0;\n")
        .parse()
        .unwrap();
    book.write_concept(
        Namespace::Styleguide,
        &"styleguide/shared-rule".parse().unwrap(),
        &rule,
    )
    .unwrap();
    let local = fixture_copy("valid-acme-billing");
    let mut state = McpState::new(factory(
        local.path().to_path_buf(),
        vec![imported.path().to_path_buf()],
    ));

    let report = state
        .search_rules(RulesParams {
            cwd: project(),
            query: "variable naming conventions".to_string(),
            language: None,
            category: None,
            k: None,
        })
        .unwrap();

    let hit = report
        .hits
        .iter()
        .find(|h| h.argosy == "acme-shared")
        .expect("imported rule is a hit");
    assert_eq!(hit.kind, "imported");
    assert_eq!(
        hit.trust, "unverified",
        "human-reviewed self-claim is floored"
    );
    let expected_open = format!(
        "<untrusted-content argosy=\"acme-shared\" uri=\"{}\" trust=\"unverified\">",
        hit.uri
    );
    let good = hit.good.as_deref().expect("good excerpt present");
    assert!(good.starts_with(&expected_open), "got {good}");
    assert!(good.ends_with("</untrusted-content>"));
    assert!(good.contains("let retry_count = 0;"));
    let bad = hit.bad.as_deref().expect("bad excerpt present");
    assert!(bad.starts_with(&expected_open), "got {bad}");
    assert!(bad.ends_with("</untrusted-content>"));
    assert!(bad.contains("let retryCount = 0;"));

    // The local rule's excerpts stay bare — same query, same sections.
    let local_hit = report
        .hits
        .iter()
        .find(|h| h.concept_id.ends_with("snake-case-vars"))
        .expect("local rule is a hit");
    assert_eq!(local_hit.kind, "local");
    assert_eq!(local_hit.good.as_deref(), Some("let retry_count = 0;"));
    assert_eq!(local_hit.bad.as_deref(), Some("let retryCount = 0;"));
}
