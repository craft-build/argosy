//! Search semantics: namespace/facet filters, cross-argosy scoping.

use super::*;

// --- Namespace + facet filters ---

#[test]
fn search_against_a_store_built_with_a_different_width_is_a_rebuild_hint() {
    let (_local, _imported, ctx) = fixture();
    // A 128-dim embedder against a store recorded at 64 is the stale half of a model switch.
    let index = Index::new(
        MockEmbedder::new(),
        MemStore::new().with_recorded_dimensions(64),
    );
    let err = index
        .search(&ctx, &Query::unscoped("architecture", 10))
        .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("different embedding model"), "{msg}");
    assert!(msg.contains("argosy index build"), "{msg}");

    // Matching width searches normally; unknown width is not an error.
    let index = Index::new(
        MockEmbedder::new(),
        MemStore::new().with_recorded_dimensions(128),
    );
    assert!(
        index
            .search(&ctx, &Query::unscoped("architecture", 10))
            .is_ok()
    );
    let index = Index::new(MockEmbedder::new(), MemStore::new());
    assert!(
        index
            .search(&ctx, &Query::unscoped("architecture", 10))
            .is_ok()
    );

    // Unknown width *with* units is refused, never silently allowed.
    let mut stale = MemStore::new();
    stale.forget_dimensions_but_keep_units();
    let index = Index::new(MockEmbedder::new(), stale);
    let err = index
        .search(&ctx, &Query::unscoped("architecture", 10))
        .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("dimensionality is unrecorded"), "{msg}");
    assert!(msg.contains("argosy index build"), "{msg}");
}

#[test]
fn namespace_filter_scopes_search_to_the_selected_namespaces() {
    let (_local, _imported, ctx) = fixture();
    let mut index = fresh_index();
    index.reconcile(&ctx).unwrap();

    // Search is query-side: asymmetric models depend on the `embed_query`/`embed` split.
    let (q_before, e_before) = (
        index.provider().query_embed_calls(),
        index.provider().embed_calls(),
    );
    let hits = index
        .search(&ctx, &Query::unscoped("naming case style", 10))
        .unwrap();
    assert!(!hits.is_empty());
    assert_eq!(index.provider().query_embed_calls(), q_before + 1);
    assert_eq!(
        index.provider().embed_calls(),
        e_before + 1,
        "default embed_query delegates to embed"
    );

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

    // Semantic + structured in one call.
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
    // Identical text in both argosies must tie exactly — the search path never sees precedence.
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

    // The two memory notes share identical text, so only the walk rule can separate them.
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

    // Explicit memory selection overrides the default local-only memory rule.
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

// --- Hybrid fusion: the reciprocal-rank fuse over leg rankings ---

/// A minimal hit keyed for the fuse: identity, passage, score.
fn fuse_hit(argosy: &str, id: &str, chunk_ordinal: u32, score: f32) -> SearchHit {
    SearchHit {
        concept: QualifiedConceptId {
            argosy: argosy.to_string(),
            namespace: Namespace::Document,
            id: concept_id(id),
        },
        score,
        meta: UnitMeta::default(),
        chunk_ordinal,
        heading_path: String::new(),
        start_line: 0,
        end_line: 0,
        passage: String::new(),
    }
}

#[test]
fn fusing_one_leg_plus_an_empty_leg_keeps_the_legs_ranking() {
    let leg = vec![
        fuse_hit("local", "document/a", 0, 0.9),
        fuse_hit("local", "document/b", 0, 0.4),
    ];
    // A lone leg — or one beside an empty leg, the vector-only default — keeps its order.
    for fused in [
        rrf_fuse(std::slice::from_ref(&leg), 10),
        rrf_fuse(&[leg, Vec::new()], 10),
    ] {
        assert_eq!(fused.len(), 2);
        assert_eq!(fused[0].concept.id, concept_id("document/a"));
        assert!(fused[0].score > fused[1].score);
        assert!(
            (fused[0].score as f64 - 1.0 / f64::from(RRF_K)).abs() < 1e-6,
            "rank 0 scores 1/(RRF_K + 0): {}",
            fused[0].score
        );
    }
    assert!(rrf_fuse(&[], 10).is_empty(), "no legs: no hits");
    assert!(
        rrf_fuse(&[vec![fuse_hit("local", "document/a", 0, 1.0)]], 0).is_empty(),
        "k = 0"
    );
}

#[test]
fn fusing_never_lets_an_argosy_precedence_reappear_in_ties() {
    // Tied leg scores stay exact ties in the fused scores (QRY-7), despite positional ranks.
    let leg = vec![
        fuse_hit("local", "document/shared", 0, 0.5),
        fuse_hit("vendor", "document/shared", 0, 0.5),
    ];
    let fused = rrf_fuse(&[leg], 10);
    assert_eq!(fused.len(), 2);
    assert_eq!(
        fused[0].score, fused[1].score,
        "tied leg scores stay fused-score ties"
    );
}

#[test]
fn fusing_boosts_the_units_both_legs_agree_on() {
    // The exact-term doc is rank 1 on the vector leg and rank 0 on the lexical leg.
    let fused = rrf_fuse(
        &[
            vec![
                fuse_hit("local", "document/nearest", 0, 0.95),
                fuse_hit("local", "document/exact", 0, 0.60),
                fuse_hit("local", "document/other", 0, 0.50),
            ],
            vec![fuse_hit("local", "document/exact", 0, -1.5)],
        ],
        10,
    );
    assert_eq!(
        fused[0].concept.id,
        concept_id("document/exact"),
        "two legs agreeing outrank one leg's lone opinion"
    );
    assert_eq!(fused[1].concept.id, concept_id("document/nearest"));
    // Score scales: exact = 1/61 + 1/60; nearest = 1/60.
    assert!(
        (fused[0].score as f64 - (1.0 / 61.0 + 1.0 / 60.0)).abs() < 1e-6,
        "contributions sum across legs: {}",
        fused[0].score
    );
}

#[test]
fn fusing_treats_passages_of_one_concept_as_separate_units() {
    // Chunk ordinals are part of the key — only agreement on the SAME passage boosts.
    let fused = rrf_fuse(
        &[
            vec![
                fuse_hit("local", "document/long", 0, 0.9),
                fuse_hit("local", "document/long", 1, 0.8),
            ],
            vec![fuse_hit("local", "document/long", 1, 0.0)],
        ],
        10,
    );
    assert!(fused[0].score > fused[1].score);
    assert_eq!(fused[0].chunk_ordinal, 1, "the twice-ranked passage wins");
    assert!(
        fused[0].score < 0.017 + 0.017,
        "fusion replaced the leg score with the fused scale"
    );
}

#[test]
fn fusing_truncates_only_after_the_fusion() {
    // Truncation happens after the fusion: hits beyond a leg's top-k still reach the head.
    let vector_leg: Vec<SearchHit> = (0..40)
        .map(|i| {
            fuse_hit(
                "local",
                &format!("document/v{i}"),
                0,
                1.0 - i as f32 / 100.0,
            )
        })
        .collect();
    let lexical_leg = vec![fuse_hit("local", "document/lexical-only", 0, -2.0)];
    let fused = rrf_fuse(&[vector_leg, lexical_leg], 2);
    assert_eq!(fused.len(), 2);
    assert!(
        fused
            .iter()
            .any(|h| h.concept.id == concept_id("document/lexical-only")),
        "the lexical-only hit survives truncation of the larger leg"
    );
}
