//! Store behavior: schema, upserts, filters, removal, upgrades.

use super::*;

#[test]
fn open_creates_schema_and_sets_user_version() {
    let (_dir, store) = open_in_tmp();
    let user_version: i64 = store
        .conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(user_version, SCHEMA_VERSION as i64);
    let journal: String = store
        .conn
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(journal, "wal");
}

#[test]
fn recorded_identity_and_dimensions_survive_reopen() {
    let (_dir, mut store) = open_in_tmp();
    assert_eq!(store.model_id(), None);
    let embedder = MockEmbedder::new();
    let unit = make_unit(
        &embedder,
        "local",
        Namespace::Document,
        "document/a",
        "persist me",
        UnitMeta::default(),
    );
    store.upsert(&[unit]).unwrap();
    store.set_model_id("mock-embedder@1").unwrap();
    let path_holder = _dir.path().join(".argosy/index.db");
    drop(store);

    let store = SqliteVecStore::open(&path_holder).unwrap();
    assert_eq!(store.model_id(), Some("mock-embedder@1"));
    assert_eq!(store.dimensions, Some(128));
    assert_eq!(store.unit_hashes().unwrap().len(), 1);
}

#[test]
fn upsert_then_search_returns_nearest_text_first() {
    let (_dir, mut store) = open_in_tmp();
    let embedder = MockEmbedder::new();
    let units = seed(&mut store, &embedder);

    let query = embedder
        .embed(&["water flows downhill".to_string()])
        .unwrap()
        .remove(0);
    let hits = store.search(&query, 3, &Filter::default()).unwrap();

    assert_eq!(hits.len(), 3);
    assert_eq!(hits[0].concept, units[0].concept, "nearest text is first");
    assert!(
        hits.windows(2).all(|w| w[0].score >= w[1].score),
        "scores are descending (IDX-7)"
    );
    assert!(hits[0].score > hits[2].score, "ranking discriminates");
}

#[test]
fn upsert_replaces_a_concept_instead_of_duplicating_it() {
    let (_dir, mut store) = open_in_tmp();
    let embedder = MockEmbedder::new();
    seed(&mut store, &embedder);
    let edited = make_unit(
        &embedder,
        "local",
        Namespace::Document,
        "document/arch",
        "completely different text about birds",
        meta(Some("Note"), &["birds"], None, None),
    );
    store.upsert(std::slice::from_ref(&edited)).unwrap();

    let hashes = store.unit_hashes().unwrap();
    assert_eq!(hashes.len(), 3, "re-upsert replaces, does not duplicate");
    assert_eq!(hashes[&edited.concept], edited.text_hash);
}

#[test]
fn search_filters_on_namespace_argosy_type_and_facets() {
    let (_dir, mut store) = open_in_tmp();
    let embedder = MockEmbedder::new();
    let units = seed(&mut store, &embedder);
    let query = embedder
        .embed(&["broad query water rust naming".to_string()])
        .unwrap()
        .remove(0);

    let expect_single = |filter: Filter, want: &EmbeddingUnit, label: &str| {
        let hits = store.search(&query, 10, &filter).unwrap();
        assert_eq!(hits.len(), 1, "{label} isolates one concept");
        assert_eq!(hits[0].concept, want.concept, "{label}");
    };

    expect_single(
        Filter {
            namespaces: Some(vec![Namespace::Document]),
            ..Filter::default()
        },
        &units[0],
        "namespace filter",
    );
    expect_single(
        Filter {
            namespaces: Some(vec![Namespace::Skill]),
            argosies: Some(vec!["local".to_string()]),
            ..Filter::default()
        },
        &units[1],
        "argosy ∧ namespace filter",
    );
    expect_single(
        Filter {
            concept_types: Some(vec!["Styleguide Rule".to_string()]),
            ..Filter::default()
        },
        &units[2],
        "concept_type filter",
    );
    expect_single(
        Filter {
            tags: Some(vec!["geo".to_string(), "style".to_string()]),
            language: Some("rust".to_string()),
            category: Some("naming".to_string()),
            ..Filter::default()
        },
        &units[2],
        "tags/language/category filter",
    );
}

#[test]
fn remove_concept_drops_every_trace_of_it() {
    let (_dir, mut store) = open_in_tmp();
    let embedder = MockEmbedder::new();
    let units = seed(&mut store, &embedder);
    store.remove_concept(&units[0].concept).unwrap();

    assert_eq!(store.unit_hashes().unwrap().len(), 2);
    let query = embedder
        .embed(&["water flows downhill".to_string()])
        .unwrap()
        .remove(0);
    let hits = store.search(&query, 10, &Filter::default()).unwrap();
    assert!(
        hits.iter().all(|h| h.concept != units[0].concept),
        "a removed concept is no longer retrievable"
    );
}

#[test]
fn clear_empties_and_stays_operational() {
    let (_dir, mut store) = open_in_tmp();
    let embedder = MockEmbedder::new();
    seed(&mut store, &embedder);
    store.set_model_id("mock-embedder@1").unwrap();

    store.clear().unwrap();

    assert_eq!(
        store.model_id(),
        None,
        "clear drops identity (rebuild re-stamps)"
    );
    assert!(store.unit_hashes().unwrap().is_empty());
    assert_eq!(
        store.dimensions, None,
        "clear releases the dimensionality so a differently-sized model can rebuild"
    );
    let query = embedder
        .embed(&["water flows downhill".to_string()])
        .unwrap()
        .remove(0);
    assert!(
        store
            .search(&query, 10, &Filter::default())
            .unwrap()
            .is_empty()
    );

    // And the store is immediately usable again (the rebuild path):
    // the re-seed re-establishes identity-less dimensionality in the same
    // transaction as its inserts.
    seed(&mut store, &embedder);
    assert_eq!(store.unit_hashes().unwrap().len(), 3);
    assert_eq!(store.dimensions, Some(128));
    // The re-established vec table is queryable immediately.
    let hits = store.search(&query, 10, &Filter::default()).unwrap();
    assert_eq!(hits.len(), 3);
}

/// Regression (review P1): a batch holding several chunks of ONE concept
/// must keep all of them — the delete is per concept, not per unit.
#[test]
fn upsert_keeps_every_chunk_of_a_multi_chunk_concept() {
    let (_dir, mut store) = open_in_tmp();
    let embedder = MockEmbedder::new();
    let mut chunk0 = make_unit(
        &embedder,
        "local",
        Namespace::Document,
        "document/arch",
        "water flows downhill through valleys",
        meta(Some("Note"), &["geo"], None, None),
    );
    let mut chunk1 = chunk0.clone();
    chunk0.chunk_ordinal = 0;
    chunk1.chunk_ordinal = 1;
    chunk1.vector = embedder
        .embed(&["distant mountains rivers oceans".to_string()])
        .unwrap()
        .remove(0);
    store.upsert(&[chunk0.clone(), chunk1.clone()]).unwrap();

    let units_rows: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM units", [], |row| row.get(0))
        .unwrap();
    let vec_rows: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM unit_vectors", [], |row| row.get(0))
        .unwrap();
    assert_eq!(units_rows, 2, "both chunks kept");
    assert_eq!(vec_rows, 2, "every chunk has exactly one vector");
    assert_eq!(store.unit_hashes().unwrap().len(), 1, "one concept");

    // And re-upserting the pair remains stable (no growth, no loss).
    store.upsert(&[chunk0, chunk1]).unwrap();
    let units_rows: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM units", [], |row| row.get(0))
        .unwrap();
    assert_eq!(units_rows, 2, "re-upsert replaces both chunks in place");
}

/// Regression (review P1): a provider whose model has a DIFFERENT vector
/// width must be able to rebuild through mismatch → clear → upsert, not
/// wedge on the store's old dimensionality forever.
#[test]
fn model_upgrade_with_a_different_dimension_rebuilds_cleanly() {
    let (_dir, mut store) = open_in_tmp();
    let embedder = MockEmbedder::new();
    seed(&mut store, &embedder); // 128-dim
    store.set_model_id("mock-embedder@1").unwrap();

    // The engine's mismatch path.
    store.clear().unwrap();
    let mut moved = make_unit(
        &embedder,
        "local",
        Namespace::Document,
        "document/arch",
        "water flows downhill through valleys",
        meta(Some("Note"), &["geo"], None, None),
    );
    moved.vector = vec![0.25f32; 64]; // a new model width
    store.upsert(&[moved]).unwrap();
    store.set_model_id("mock-embedder@2").unwrap();

    assert_eq!(store.dimensions, Some(64), "the new width is adopted");
    let hits = store
        .search(&[0.25f32; 64], 10, &Filter::default())
        .unwrap();
    assert_eq!(
        hits.len(),
        1,
        "rebuilt store is searchable at the new width"
    );
}

/// Regression (review P2): empty allow-list filters return no hits, never
/// a SQLite syntax error — parity with the doc-06 reference semantics.
#[test]
fn empty_filter_lists_return_no_hits_not_an_error() {
    let (_dir, mut store) = open_in_tmp();
    let embedder = MockEmbedder::new();
    seed(&mut store, &embedder);
    let query = embedder
        .embed(&["water flows downhill".to_string()])
        .unwrap()
        .remove(0);

    for filter in [
        Filter {
            namespaces: Some(vec![]),
            ..Filter::default()
        },
        Filter {
            argosies: Some(vec![]),
            ..Filter::default()
        },
        Filter {
            concept_types: Some(vec![]),
            ..Filter::default()
        },
        Filter {
            tags: Some(vec![]),
            ..Filter::default()
        },
    ] {
        assert!(
            store.search(&query, 10, &filter).unwrap().is_empty(),
            "an empty allow-list matches nothing, without error"
        );
    }
}

/// Regression (review P3): a db written by a NEWER argosy must fail
/// loudly instead of being misread as v1 (and its version must not be
/// clobbered).
#[test]
fn opening_a_newer_schema_version_is_a_loud_error() {
    let (dir, store) = open_in_tmp();
    store
        .conn
        .pragma_update(None, "user_version", SCHEMA_VERSION as i64 + 1)
        .unwrap();
    let path = dir.path().join(".argosy/index.db");
    drop(store);

    let err = SqliteVecStore::open(&path).err().expect("open should fail");
    assert!(
        err.to_string().contains("newer"),
        "names the schema-version mismatch: {err}"
    );
    let version_held: i64 = rusqlite::Connection::open(&path)
        .unwrap()
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        version_held,
        SCHEMA_VERSION as i64 + 1,
        "the newer version is never clobbered"
    );
}

#[test]
fn wrong_dimension_upsert_is_an_error_not_a_truncation() {
    let (_dir, mut store) = open_in_tmp();
    let embedder = MockEmbedder::new();
    let units = seed(&mut store, &embedder);
    let mut bad = units[0].clone();
    bad.vector = vec![0.5f32; 64];

    let err = store.upsert(&[bad]).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("64") && msg.contains("128"),
        "error names both dimensions: {msg}"
    );
    assert_eq!(
        store.unit_hashes().unwrap().len(),
        3,
        "a rejected upsert stores nothing"
    );

    let err = store
        .search(&[0.0f32; 64], 5, &Filter::default())
        .unwrap_err();
    assert!(err.to_string().contains("dimension"));
}

#[test]
fn unit_hashes_round_trip_tracks_every_mutation() {
    let (_dir, mut store) = open_in_tmp();
    let embedder = MockEmbedder::new();
    let units = seed(&mut store, &embedder);
    let hashes = store.unit_hashes().unwrap();
    assert_eq!(hashes.len(), 3);
    for unit in &units {
        assert_eq!(hashes[&unit.concept], unit.text_hash);
    }
    store.remove_concept(&units[1].concept).unwrap();
    assert_eq!(store.unit_hashes().unwrap().len(), 2);
}
