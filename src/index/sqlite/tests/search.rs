//! Search semantics and end-to-end reconciliation.

use super::*;

/// Regression (filtered recall): vec0 applies metadata filters after
/// whenever the k nearest concepts are all non-matching. The store
/// ranks the full corpus when a filter is active; filtered queries
/// are exact.
#[test]
fn filtered_search_returns_matches_beyond_the_unfiltered_top_k() {
    let (_dir, mut store) = open_in_tmp();
    let embedder = MockEmbedder::new();
    // Ten near-duplicate documents close to the query …
    let mut units: Vec<EmbeddingUnit> = (0..10)
        .map(|i| {
            make_unit(
                &embedder,
                "local",
                Namespace::Document,
                &format!("document/d{i}"),
                "water flows downhill through valleys",
                meta(Some("Note"), &[], None, None),
            )
        })
        .collect();
    // … plus one styleguide rule that is NOT among the nearest.
    units.push(make_unit(
        &embedder,
        "local",
        Namespace::Styleguide,
        "styleguide/rust/naming",
        "naming conventions snake case identifiers",
        meta(Some("Styleguide Rule"), &[], Some("rust"), Some("naming")),
    ));
    store.upsert(&units).unwrap();

    let query = embedder
        .embed(&["water flows downhill".to_string()])
        .unwrap()
        .remove(0);

    // k = 3 with a styleguide filter: the unfiltered top-3 are all
    // documents, yet the rule must surface — not an empty result.
    let hits = store
        .search(
            &query,
            3,
            &Filter {
                namespaces: Some(vec![Namespace::Styleguide]),
                ..Filter::default()
            },
        )
        .unwrap();
    assert_eq!(hits.len(), 1, "the only matching concept is returned");
    assert_eq!(hits[0].concept.id.as_str(), "styleguide/rust/naming");

    // The facet path (the review flow: `search_rules --language rust`).
    let hits = store
        .search(
            &query,
            3,
            &Filter {
                language: Some("rust".to_string()),
                ..Filter::default()
            },
        )
        .unwrap();
    assert_eq!(hits.len(), 1, "language facet finds the same rule");

    // Unfiltered, the same query honors k exactly — nothing regressed.
    let hits = store.search(&query, 3, &Filter::default()).unwrap();
    assert_eq!(hits.len(), 3);
    assert!(
        hits.iter()
            .all(|h| h.concept.namespace == Namespace::Document)
    );
}

/// Real-database coverage: engine + store on a temp ProjectContext,
/// including the model-mismatch rebuild paths.
#[test]
fn reconcile_end_to_end_with_sqlite_store() {
    let (local, _imported, ctx) = fixture();
    let db_path = local.path().join(".argosy/index.db");
    let mut index = Index::new(MockEmbedder::new(), SqliteVecStore::open(&db_path).unwrap());

    let report = index.reconcile(&ctx).unwrap();
    assert_eq!(report.upserted, 5);
    assert_eq!(report.model_id, "mock-embedder@1");

    // Search over real SQL: the architecture doc wins its own query.
    let hits = index
        .search(&ctx, &Query::unscoped("architecture", 10))
        .unwrap();
    assert!(
        hits.iter()
            .any(|h| h.concept.id.as_str() == "document/arch" && h.concept.argosy == "local")
    );
    assert_eq!(hits[0].concept.id.as_str(), "document/arch");

    // Edit one concept → exactly it is re-staged, and its old text is gone.
    let edited = crate::concept::Concept::from_str(
        "---\ntype: Note\ndescription: Build gotchas.\ntags: [build]\n---\nMoldova espresso deadlines.\n",
    )
    .unwrap();
    ctx.local()
        .write_concept(
            Namespace::Memory,
            &"memory/gotchas".parse().unwrap(),
            &edited,
        )
        .unwrap();
    let report = index.reconcile(&ctx).unwrap();
    assert_eq!(report.upserted, 1);
    assert_eq!(report.unchanged, 4);

    let hits = index
        .search(&ctx, &Query::unscoped("moldova espresso", 10))
        .unwrap();
    assert_eq!(hits[0].concept.id.as_str(), "memory/gotchas");
    let hits = index
        .search(&ctx, &Query::unscoped("cargo needs lockfile", 10))
        .unwrap();
    assert_ne!(
        hits[0].concept.id.as_str(),
        "memory/gotchas",
        "the replaced text no longer tops its own old query"
    );

    // A provider with a different identity rebuilds
    // everything with zero errors and zero mixed vectors.
    index.set_provider(MockEmbedder::with_model_id(
        "tract/sentence-transformers/all-MiniLM-L6-v2@tract-9",
    ));
    let report = index.reconcile(&ctx).unwrap();
    assert!(report.rebuilt);
    assert_eq!(report.upserted, 5);
    assert_eq!(
        index.store().model_id(),
        Some("tract/sentence-transformers/all-MiniLM-L6-v2@tract-9")
    );
}

/// A second open over the same db reuses it with zero re-embeds.
#[test]
fn reopening_the_store_reuses_the_index_with_zero_reembeds() {
    let (_local, _imported, ctx) = fixture();
    let db_dir = TempDir::new().unwrap();
    let db_path = db_dir.path().join(".argosy/index.db");
    let mut index = Index::new(MockEmbedder::new(), SqliteVecStore::open(&db_path).unwrap());
    index.reconcile(&ctx).unwrap();
    drop(index);

    let mut index = Index::new(MockEmbedder::new(), SqliteVecStore::open(&db_path).unwrap());
    assert_eq!(
        index.store().model_id(),
        Some("mock-embedder@1"),
        "the recorded identity survives the reopen"
    );
    let report = index.reconcile(&ctx).unwrap();
    assert!(!report.rebuilt);
    assert_eq!(report.upserted, 0);
    assert_eq!(report.unchanged, 5);
    assert_eq!(
        index.provider().embed_calls(),
        0,
        "NFR-4: nothing re-embedded"
    );
}

/// Passage fields survive the sqlite round trip: the matched chunk's
/// ordinal, breadcrumb, line range and text come back exactly as upserted.
#[test]
fn search_returns_the_matched_passage_fields() {
    let (_dir, mut store) = open_in_tmp();
    let embedder = MockEmbedder::new();
    let mut chunk = make_unit(
        &embedder,
        "local",
        Namespace::Document,
        "document/arch",
        "Concept: Architecture\nSection: Storage\n\ndatabase locking and retries",
        meta(Some("Note"), &["db"], None, None),
    );
    chunk.chunk_ordinal = 3;
    chunk.heading_path = "Storage › Retries".to_string();
    chunk.start_line = 12;
    chunk.end_line = 20;
    store.upsert(&[chunk.clone()]).unwrap();

    let query = embedder
        .embed(&["database locking and retries".to_string()])
        .unwrap()
        .remove(0);
    let hits = store.search(&query, 1, &Filter::default()).unwrap();
    assert_eq!(hits.len(), 1);
    let hit = &hits[0];
    assert_eq!(hit.chunk_ordinal, 3);
    assert_eq!(hit.heading_path, "Storage › Retries");
    assert_eq!((hit.start_line, hit.end_line), (12, 20));
    assert_eq!(hit.passage, chunk.passage);
}

// --- The lexical leg: BM25 over the FTS5 index ---

#[test]
fn lexical_search_ranks_exact_terms_first() {
    let (_dir, mut store) = open_in_tmp();
    let embedder = MockEmbedder::new();
    let units = vec![
        make_unit(
            &embedder,
            "local",
            Namespace::Document,
            "document/err",
            "error code USR-1042 raised by the auth module",
            meta(None, &[], None, None),
        ),
        make_unit(
            &embedder,
            "local",
            Namespace::Document,
            "document/other",
            "water flows downhill through valleys",
            meta(None, &[], None, None),
        ),
    ];
    store.upsert(&units).unwrap();

    // BM25 finds the identifier that embeddings would have to compress
    // into an unstructured neighborhood.
    let hits = store
        .lexical_search("USR-1042", 10, &Filter::default())
        .unwrap();
    assert!(!hits.is_empty());
    assert_eq!(hits[0].concept.id.as_str(), "document/err");
    assert_eq!(
        hits[0].passage,
        "error code USR-1042 raised by the auth module"
    );
    // Multi-token queries OR their terms: the flow doc matches "flows".
    let hits = store
        .lexical_search("USR-1042 flows", 10, &Filter::default())
        .unwrap();
    assert_eq!(hits.len(), 2, "one hit per matching unit");
    assert_eq!(
        hits[0].concept.id.as_str(),
        "document/err",
        "two terms beat one"
    );
}

#[test]
fn lexical_search_matches_the_concept_id_column() {
    let (_dir, mut store) = open_in_tmp();
    let embedder = MockEmbedder::new();
    store
        .upsert(&[make_unit(
            &embedder,
            "local",
            Namespace::Styleguide,
            "styleguide/rust/naming/case",
            "never abbreviate identifiers",
            meta(Some("Styleguide Rule"), &[], Some("rust"), Some("naming")),
        )])
        .unwrap();

    // The passage never spells the rule id out; the indexed concept_id
    // column still makes the documented query ("the naming/case rule")
    // findable — the roadmap's exact-rule-id case.
    let hits = store
        .lexical_search("naming case", 10, &Filter::default())
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].concept.id.as_str(), "styleguide/rust/naming/case");
}

#[test]
fn lexical_search_honors_filters_like_the_vector_leg() {
    let (_dir, mut store) = open_in_tmp();
    let embedder = MockEmbedder::new();
    let units = vec![
        make_unit(
            &embedder,
            "local",
            Namespace::Styleguide,
            "styleguide/rust/naming",
            "naming conventions snake case identifiers",
            meta(Some("Styleguide Rule"), &[], Some("rust"), Some("naming")),
        ),
        make_unit(
            &embedder,
            "local",
            Namespace::Document,
            "document/naming",
            "naming things is hard",
            meta(None, &[], None, None),
        ),
    ];
    store.upsert(&units).unwrap();

    // Facet filter: only the rule declares language rust.
    let hits = store
        .lexical_search(
            "naming",
            10,
            &Filter {
                language: Some("rust".to_string()),
                ..Filter::default()
            },
        )
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].concept.id.as_str(), "styleguide/rust/naming");
    // Empty allow-list: nothing, never everything (doc-06 semantics).
    let hits = store
        .lexical_search(
            "naming",
            10,
            &Filter {
                argosies: Some(vec![]),
                ..Filter::default()
            },
        )
        .unwrap();
    assert!(hits.is_empty());
}

#[test]
fn queries_without_indexable_terms_sit_the_lexical_leg_out() {
    let (_dir, mut store) = open_in_tmp();
    let embedder = MockEmbedder::new();
    seed(&mut store, &embedder);

    // Punctuation-only, empty queries: no term, no ranking — the vector
    // leg owns the query.
    for query in ["", "   ", "?!/-", "—···"] {
        let hits = store.lexical_search(query, 10, &Filter::default()).unwrap();
        assert!(hits.is_empty(), "query {query:?} ranked nothing tokenless");
    }

    // FTS operators in the text are never parsed as syntax: quoted terms
    // mean exactly themselves, so `OR` matches no passage that does not
    // contain the literal term — and the punctuation around operators
    // ("OR", "NEAR(a b)", "-rust") merely splits into literal terms.
    let hits = store.lexical_search("OR", 10, &Filter::default()).unwrap();
    assert!(hits.is_empty(), "the operator is a term, not syntax");
    let hits = store
        .lexical_search("naming OR case", 10, &Filter::default())
        .unwrap();
    assert_eq!(hits[0].concept.id.as_str(), "styleguide/rust/naming");
    let hits = store
        .lexical_search("-rust", 10, &Filter::default())
        .unwrap();
    assert_eq!(hits[0].concept.id.as_str(), "skill/deploy");
}

#[test]
fn lexical_search_tracks_writes_through_the_units_triggers() {
    let (_dir, mut store) = open_in_tmp();
    let embedder = MockEmbedder::new();
    let unit = make_unit(
        &embedder,
        "local",
        Namespace::Document,
        "document/volatile",
        "quokka habitats and diet",
        meta(None, &[], None, None),
    );
    store.upsert(&[unit]).unwrap();
    assert_eq!(
        store
            .lexical_search("quokka", 10, &Filter::default())
            .unwrap()
            .len(),
        1
    );

    // Re-upsert with different text: the FTS index must follow the
    // delete-then-insert triggers, leaving no stale term behind.
    let rewritten = make_unit(
        &embedder,
        "local",
        Namespace::Document,
        "document/volatile",
        "wallaby habitats instead",
        meta(None, &[], None, None),
    );
    store.upsert(&[rewritten]).unwrap();
    assert!(
        store
            .lexical_search("quokka", 10, &Filter::default())
            .unwrap()
            .is_empty(),
        "the replaced passage's terms are gone from the index"
    );
    assert_eq!(
        store
            .lexical_search("wallaby", 10, &Filter::default())
            .unwrap()
            .len(),
        1
    );

    // Removal and clear stay in sync too.
    store
        .remove_concept(&QualifiedConceptId {
            argosy: "local".to_string(),
            namespace: Namespace::Document,
            id: "document/volatile".parse().unwrap(),
        })
        .unwrap();
    assert!(
        store
            .lexical_search("wallaby", 10, &Filter::default())
            .unwrap()
            .is_empty()
    );
    store
        .upsert(&[make_unit(
            &embedder,
            "local",
            Namespace::Document,
            "document/c",
            "wallaby returns",
            meta(None, &[], None, None),
        )])
        .unwrap();
    store.clear().unwrap();
    assert!(
        store
            .lexical_search("wallaby", 10, &Filter::default())
            .unwrap()
            .is_empty(),
        "clear empties the FTS index with the rest"
    );
}

/// Hybrid end to end: a query whose tokens overlap no passage (so the
/// mock vector leg scores every unit identically) still finds the rule
/// through the lexical leg, and the fusion puts it first.
#[test]
fn hybrid_search_finds_rule_ids_the_vector_leg_cannot() {
    let local = make_argosy(
        "local",
        &[
            (
                "styleguide/rust/naming/case.md",
                "---\ntype: Styleguide Rule\ndescription: Identifiers.\nlanguage: rust\ncategory: \
                 naming\n---\nnever abbreviate identifiers\n",
            ),
            (
                "document/flow.md",
                "---\ntype: Note\n---\nwater flows downhill\n",
            ),
        ],
    );
    let ctx = ProjectContext::open(local.path(), []).unwrap();
    let db_path = local.path().join(".argosy/index.db");
    let mut index = Index::new(MockEmbedder::new(), SqliteVecStore::open(&db_path).unwrap());
    index.reconcile(&ctx).unwrap();

    let hits = index
        .search(&ctx, &Query::unscoped("naming case", 10))
        .unwrap();
    assert!(
        !hits.is_empty(),
        "the lexical leg answers a query the vectors cannot rank"
    );
    assert_eq!(hits[0].concept.id.as_str(), "styleguide/rust/naming/case");
}

/// The zero-model mode: a lexical-only index stores units and FTS rows
/// (no vec0 table at all, nothing ever embedded), still searches, and a
/// later switch to a real model is an ordinary full rebuild.
#[test]
fn lexical_only_mode_builds_a_zero_model_bm25_index() {
    let (_local, _imported, ctx) = fixture();
    let db_dir = TempDir::new().unwrap();
    let db_path = db_dir.path().join(".argosy/index.db");
    let mut index = Index::new(LexicalOnlyProvider, SqliteVecStore::open(&db_path).unwrap());

    let report = index.reconcile(&ctx).unwrap();
    assert_eq!(report.upserted, 5, "every concept is chunked and staged");
    assert_eq!(
        report.model_id, LEXICAL_ONLY_MODEL_ID,
        "the lexical mode records its own identity"
    );

    // No vec table exists and the vector leg is empty by construction.
    let vec_tables: i64 = index
        .store()
        .conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'unit_vectors'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(vec_tables, 0, "no vectors: nothing to store them in");
    let hits = index
        .search(&ctx, &Query::unscoped("locking retries", 10))
        .unwrap();
    assert!(
        hits.iter()
            .any(|h| h.concept.id.as_str() == "document/locking"),
        "BM25-only search still finds the locking note"
    );

    // Switching to a real model is the ordinary identity flip: full
    // rebuild, now with vectors — hybrid from then on.
    drop(index);
    let mut index = Index::new(MockEmbedder::new(), SqliteVecStore::open(&db_path).unwrap());
    let report = index.reconcile(&ctx).unwrap();
    assert!(report.rebuilt, "identity change rebuilds the whole index");
    assert_eq!(report.upserted, 5);
    let vec_tables: i64 = index
        .store()
        .conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'unit_vectors'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(vec_tables, 1, "the rebuilt index has its vec0 table back");
    let hits = index
        .search(&ctx, &Query::unscoped("locking retries", 10))
        .unwrap();
    assert!(
        hits.iter()
            .any(|h| h.concept.id.as_str() == "document/locking"),
        "the rebuilt hybrid index still finds it"
    );
}
