//! Search tools: `search`, `search_rules`, and `read`.

use super::*;

#[test]
fn search_k_defaults_to_eight_and_every_filter_field_maps() {
    let mut rig = rig();
    // The fixture index holds 8 units; a ninth makes the default k=8 observable.
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
    // A fresh state so its factory reconciles an index over the new unit count.
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
    assert_eq!(
        hit.priority.as_deref(),
        Some("error"),
        "the fixture rule declares priority: error"
    );

    // Plain `search` does not enrich — the examples are the `search_rules` contract.
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
    assert!(
        hit.priority.is_none(),
        "priority is a search_rules enrichment"
    );
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
            start_line: None,
            end_line: None,
        })
        .unwrap();
    assert_eq!(local.argosy, "acme-billing");
    assert_eq!(local.kind, "local");
    assert_eq!(local.uri, "argosy://acme-billing/memory/gotchas");
    assert!(local.content.contains("# Gotchas"));

    // Reads can reach imported argosys; resources only serve the cwd's project.
    let imported = rig
        .state
        .read(ReadParams {
            cwd: project(),
            path: "skill/shared-audit".to_string(),
            argosy: Some("acme-shared".to_string()),
            start_line: None,
            end_line: None,
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
            start_line: None,
            end_line: None,
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
    // Without a configured cap, an import's `verified` self-claim must not relay.
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
    // The cap is a ceiling, not a grant — claims at or below it stand; higher ones are capped.
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
    // A fresh state so its factory reconciles an index including the write.
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
            start_line: None,
            end_line: None,
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
            start_line: None,
            end_line: None,
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
    // Good/Bad excerpts reach the reviewer prompt, so they get full-body delimiters too.
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

// --- Passage-level hits and paid ranged reads ---

#[test]
fn hits_carry_passage_heading_and_line_range() {
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
    let hit = report
        .hits
        .iter()
        .find(|h| h.uri == "argosy://acme-billing/document/rate-limit-behavior")
        .expect("the rate-limit note is a hit");
    assert!(
        hit.passage.contains("rate limit")
            || hit.passage.contains("Rate limit")
            || hit.passage.contains("Rate Limit"),
        "the passage carries the matched text: {}",
        hit.passage
    );
    assert!(hit.passage.starts_with("Concept: "), "{}", hit.passage);
    assert_eq!((hit.start_line, hit.end_line), (1, 4));
    assert_eq!(hit.chunk_ordinal, 0, "a short concept is one passage");
}

#[test]
fn imported_passages_arrive_delimiter_wrapped() {
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
    let open = format!(
        "<untrusted-content argosy=\"acme-shared\" uri=\"{}\" trust=\"unverified\">",
        imported.uri
    );
    assert!(imported.passage.starts_with(&open), "{}", imported.passage);
    assert!(imported.passage.ends_with("</untrusted-content>"));
    assert!(imported.passage.contains("Steps."));

    let local = report
        .hits
        .iter()
        .find(|h| h.argosy == "acme-billing")
        .expect("local concept is a hit");
    assert!(
        !local.passage.contains("untrusted-content"),
        "local passages stay bare: {}",
        local.passage
    );
}

#[test]
fn read_slices_the_body_by_line_range() {
    let mut rig = rig();
    let full = rig
        .state
        .read(ReadParams {
            cwd: project(),
            path: "memory/gotchas".to_string(),
            argosy: None,
            start_line: None,
            end_line: None,
        })
        .unwrap();
    assert!(full.lines.is_none(), "an unranged read reports no range");

    let ranged = rig
        .state
        .read(ReadParams {
            cwd: project(),
            path: "memory/gotchas".to_string(),
            argosy: None,
            start_line: Some(3),
            end_line: Some(4),
        })
        .unwrap();
    let lines = ranged.lines.expect("ranged read reports its slice");
    assert_eq!((lines.start, lines.end), (3, 4));
    assert!(
        ranged.content.contains("The processor rate-limits"),
        "body lines 3–4: {}",
        ranged.content
    );
    assert!(
        !ranged.content.contains("# Gotchas"),
        "frontmatter and line 1 are excluded: {}",
        ranged.content
    );
    assert!(
        !ranged.content.contains("---"),
        "frontmatter is never part of the body slice: {}",
        ranged.content
    );

    // An open-ended range runs to the end; an out-of-range end clamps.
    let tail = rig
        .state
        .read(ReadParams {
            cwd: project(),
            path: "memory/gotchas".to_string(),
            argosy: None,
            start_line: Some(4),
            end_line: None,
        })
        .unwrap();
    assert!(
        tail.content
            .contains("see `document/rate-limit-behavior.md`")
    );
}

#[test]
fn read_rejects_invalid_line_ranges_with_actionable_errors() {
    let mut rig = rig();
    let base = |start, end| ReadParams {
        cwd: project(),
        path: "memory/gotchas".to_string(),
        argosy: None,
        start_line: start,
        end_line: end,
    };

    let err = rig.state.read(base(Some(0), None)).unwrap_err();
    assert!(err.to_string().contains("1-based"), "got {err}");
    let err = rig.state.read(base(Some(99), None)).unwrap_err();
    assert!(
        err.to_string().contains("past the end of the body"),
        "got {err}"
    );
    let err = rig.state.read(base(Some(4), Some(2))).unwrap_err();
    assert!(err.to_string().contains("precedes"), "got {err}");
}

#[test]
fn a_long_passage_is_capped_with_an_expansion_hint() {
    // The wire shape caps a passage and points at the ranged read rather than shipping an
    // unbounded payload.
    let capped = crate::mcp::session::cap_chars(
        &"x".repeat(crate::mcp::session::MAX_PASSAGE_CHARS + 10),
        crate::mcp::session::MAX_PASSAGE_CHARS,
    );
    assert!(
        capped.chars().count() < crate::mcp::session::MAX_PASSAGE_CHARS + 200,
        "the payload stays bounded: {} chars",
        capped.chars().count()
    );
    assert!(
        capped.contains("use `read` with this hit's start_line/end_line"),
        "the hint names the expansion path: {capped}"
    );
    // A passage under the cap is untouched.
    let short = crate::mcp::session::cap_chars("short passage", 2_000);
    assert_eq!(short, "short passage");
}

#[test]
fn imported_ranged_reads_stay_delimiter_wrapped() {
    let mut rig = rig();
    let ranged = rig
        .state
        .read(ReadParams {
            cwd: project(),
            path: "skill/shared-audit".to_string(),
            argosy: Some("acme-shared".to_string()),
            start_line: Some(1),
            end_line: Some(1),
        })
        .unwrap();
    assert_eq!(ranged.kind, "imported");
    assert!(ranged.content.starts_with("<untrusted-content"));
    assert!(ranged.content.ends_with("</untrusted-content>"));
    assert_eq!(ranged.lines.map(|l| (l.start, l.end)), Some((1, 1)));
    assert!(ranged.content.contains("# Audit"), "{}", ranged.content);
    assert!(
        !ranged.content.contains("Steps."),
        "the slice is one body line: {}",
        ranged.content
    );
}
