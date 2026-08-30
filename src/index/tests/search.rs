//! Search semantics: namespace/facet filters, cross-argosy scoping.

use super::*;

// --- Namespace + facet filters ---

#[test]
fn namespace_filter_scopes_search_to_the_selected_namespaces() {
    let (_local, _imported, ctx) = fixture();
    let mut index = fresh_index();
    index.reconcile(&ctx).unwrap();

    let mut query = Query::unscoped("naming case style", 10);
    query.filter.namespaces = Some(vec![Namespace::Styleguide]);
    let hits = index.search(&ctx, &query).unwrap();

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].concept.namespace, Namespace::Styleguide);
    assert_eq!(hits[0].concept.argosy, "local");
    assert_eq!(
        hits[0].concept.id,
        concept_id("styleguide/rust/naming/case")
    );

    // Skills only: the styleguide hit is gone.
    query.filter.namespaces = Some(vec![Namespace::Skill]);
    let hits = index.search(&ctx, &query).unwrap();
    assert!(hits.iter().all(|h| h.concept.namespace == Namespace::Skill));
}

#[test]
fn facet_filters_narrow_individually_and_compose_with_semantics() {
    let (_local, _imported, ctx) = fixture();
    let mut index = fresh_index();
    index.reconcile(&ctx).unwrap();

    let mut query = Query::unscoped("service", 10);

    // language + category facets.
    query.filter.language = Some("rust".to_string());
    let hits = index.search(&ctx, &query).unwrap();
    assert_eq!(hits.len(), 1, "only the styleguide rule declares language");
    assert_eq!(hits[0].meta.language.as_deref(), Some("rust"));
    query.filter.category = Some("naming".to_string());
    let hits = index.search(&ctx, &query).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].meta.category.as_deref(), Some("naming"));
    assert_eq!(
        hits[0].meta.concept_type.as_deref(),
        Some("Styleguide Rule")
    );
    query.filter.language = None;
    query.filter.category = None;

    // tags (any-of): only the imported locking note is tagged `database`.
    query.filter.tags = Some(vec!["database".to_string()]);
    let hits = index.search(&ctx, &query).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].concept.argosy, "vendor");
    assert!(hits[0].meta.tags.contains(&"database".to_string()));
    query.filter.tags = None;

    // concept_types: only the deploy entry point is a Skill.
    query.filter.concept_types = Some(vec!["Skill".to_string()]);
    let hits = index.search(&ctx, &query).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].concept.id, concept_id("skill/deploy"));
    query.filter.concept_types = None;

    // Semantic + structured in one call: the database-tagged
    // locking note tops a `database locking` semantic query, and the
    // namespace constraint still holds.
    let mut query = Query::unscoped("database locking retries", 10);
    query.filter.namespaces = Some(vec![Namespace::Document]);
    let hits = index.search(&ctx, &query).unwrap();
    assert!(!hits.is_empty());
    assert_eq!(hits[0].concept.argosy, "vendor");
    assert!(
        hits.iter()
            .all(|h| h.concept.namespace == Namespace::Document)
    );
    assert!(
        hits[0].score > hits[1].score,
        "ranking follows similarity to the query"
    );
}

// --- Unscoped search across argosys ---

#[test]
fn unscoped_search_spans_argosys_with_score_only_ranking_and_no_precedence_boost() {
    // Identical text in the local and an imported argosy: their scores
    // must tie exactly — any local-first reordering would require
    // precedence data the search path never receives.
    const SAME: &str = "---\ntype: Note\ndescription: Shared body.\n---\nalpha beta gamma.\n";
    let local = make_argosy("local", &[("document/shared.md", SAME)]);
    let imported = make_argosy("vendor", &[("document/shared.md", SAME)]);
    let ctx = ProjectContext::open(local.path(), [imported.path().to_path_buf()]).unwrap();
    let mut index = fresh_index();
    index.reconcile(&ctx).unwrap();

    let hits = index
        .search(&ctx, &Query::unscoped("alpha beta gamma", 10))
        .unwrap();
    assert_eq!(hits.len(), 2, "QRY-6: unscoped search spans both argosies");
    let origins: Vec<&str> = hits.iter().map(|h| h.concept.argosy.as_str()).collect();
    assert!(origins.contains(&"local") && origins.contains(&"vendor"));
    assert_eq!(
        hits[0].score, hits[1].score,
        "identical text ⇒ identical score; no precedence reweighting"
    );

    // Scoping to a single argosy by name narrows to it.
    let mut query = Query::unscoped("alpha beta gamma", 10);
    query.filter.argosies = Some(vec!["vendor".to_string()]);
    let hits = index.search(&ctx, &query).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].concept.argosy, "vendor");
}

#[test]
fn scoping_search_to_an_inactive_argosy_is_an_error_not_an_empty_result() {
    let (_local, _imported, ctx) = fixture();
    let mut index = fresh_index();
    index.reconcile(&ctx).unwrap();

    let mut query = Query::unscoped("anything", 10);
    query.filter.argosies = Some(vec!["ghost".to_string()]);
    let err = index.search(&ctx, &query).unwrap_err();
    assert!(
        matches!(err, Error::UnknownArgosy { ref name } if name == "ghost"),
        "inactive argosy names must surface a config error, got {err}"
    );
}

// --- Local memory search, imported memory excluded (default walk) ---

#[test]
fn local_memory_is_searchable_but_imported_memory_stays_out_of_the_default_walk() {
    let (_local, _imported, ctx) = fixture();
    let mut index = fresh_index();
    index.reconcile(&ctx).unwrap();

    // The two memory notes share identical text, so with no walk
    // difference both would appear.
    let hits = index
        .search(&ctx, &Query::unscoped("Cargo lockfile build gotchas", 10))
        .unwrap();
    let memory_hits: Vec<_> = hits
        .iter()
        .filter(|h| h.concept.namespace == Namespace::Memory)
        .collect();
    assert_eq!(memory_hits.len(), 1, "exactly one memory note is indexed");
    assert_eq!(memory_hits[0].concept.argosy, "local");

    // An imported argosy without memory/ contributes none either.
    let local2 = make_argosy("local2", &[]);
    let imported2 = make_argosy("plain", &[("document/x.md", DOC_ARCH)]);
    let ctx2 = ProjectContext::open(local2.path(), [imported2.path().to_path_buf()]).unwrap();
    let mut index2 = fresh_index();
    index2.reconcile(&ctx2).unwrap();
    assert!(
        index2
            .store()
            .unit_hashes()
            .unwrap()
            .keys()
            .all(|q| q.namespace != Namespace::Memory)
    );
}

// --- Explicit namespace selection (custom walk + honored imported memory) ---

#[test]
fn with_namespaces_honors_the_selection_verbatim_for_every_argosy() {
    let local = make_argosy("local", &[("document/arch.md", DOC_ARCH)]);
    let imported = make_argosy("vendor", &[("memory/vendor-notes.md", MEMORY_GOTCHAS)]);
    let ctx = ProjectContext::open(local.path(), [imported.path().to_path_buf()]).unwrap();

    // Memory only: local arch is excluded; vendor memory is included —
    // explicit selection overrides the default local-only memory rule.
    let mut index = Index::with_namespaces(
        MockEmbedder::new(),
        MemStore::new(),
        vec![Namespace::Memory],
    );
    let report = index.reconcile(&ctx).unwrap();
    assert_eq!(report.upserted, 1);
    let hashes = index.store().unit_hashes().unwrap();
    assert_eq!(hashes.len(), 1);
    let qid = hashes.keys().next().unwrap();
    assert_eq!(qid.argosy, "vendor");
    assert_eq!(qid.namespace, Namespace::Memory);
}
