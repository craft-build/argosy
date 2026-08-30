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
        "fastembed/sentence-transformers/all-MiniLM-L6-v2@fastembed-5",
    ));
    let report = index.reconcile(&ctx).unwrap();
    assert!(report.rebuilt);
    assert_eq!(report.upserted, 5);
    assert_eq!(
        index.store().model_id(),
        Some("fastembed/sentence-transformers/all-MiniLM-L6-v2@fastembed-5")
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
