//! Reconcile lifecycle tests: first run, unchanged run, incremental
//! edits, deletes, and model flips.

use super::*;

// --- staleness_report: the read-only preview must mirror reconcile ---

#[test]
fn staleness_after_reconcile_is_all_unchanged() {
    let (_local, _imported, ctx) = fixture();
    let mut index = fresh_index();
    index.reconcile(&ctx).unwrap();

    let report = staleness_report(&ctx, index.store(), "mock-embedder@1").unwrap();

    assert_eq!(report.added, 0);
    assert_eq!(report.changed, 0);
    assert_eq!(report.removed, 0);
    assert_eq!(report.unchanged, 5);
    assert!(!report.model_mismatch);
}

#[test]
fn staleness_preview_matches_the_diff_reconcile_applies() {
    let (local, _imported, ctx) = fixture();
    let mut index = fresh_index();
    index.reconcile(&ctx).unwrap();

    // Edit one concept, delete one, add one.
    write_file(local.path(), "memory/gotchas.md", "Edited build notes.\n");
    fs::remove_file(local.path().join("skill/deploy.md")).unwrap();
    write_file(
        local.path(),
        "document/new-note.md",
        "---\ntype: Note\n---\nA new note.\n",
    );

    let report = staleness_report(&ctx, index.store(), "mock-embedder@1").unwrap();

    assert_eq!(report.added, 1, "the new document counts as added");
    assert_eq!(report.changed, 1, "the edited memory counts as changed");
    assert_eq!(report.removed, 1, "the deleted skill counts as removed");
    assert_eq!(report.unchanged, 3);
    assert!(!report.model_mismatch);
}

#[test]
fn staleness_agrees_with_reconcile_on_identity_states() {
    let (_local, _imported, ctx) = fixture();
    let mut index = fresh_index();
    index.reconcile(&ctx).unwrap();

    // A recorded identity differing from the expected model ⇒ mismatch
    // (reconcile would rebuild).
    let report = staleness_report(&ctx, index.store(), "other-model@2").unwrap();
    assert!(report.model_mismatch);

    // Data present with NO recorded identity: reconcile's `None =>
    // !stored.is_empty()` guard rebuilds, so the preview must not claim
    // "up to date".
    index.store_mut().model_id = None;
    let report = staleness_report(&ctx, index.store(), "mock-embedder@1").unwrap();
    assert!(
        report.model_mismatch,
        "data without a recorded identity is a rebuild, never \"up to date\""
    );

    // An empty store with no recorded identity announces nothing; the
    // added counts carry the whole story.
    index.store_mut().clear().unwrap();
    let report = staleness_report(&ctx, index.store(), "mock-embedder@1").unwrap();
    assert!(!report.model_mismatch);
    assert_eq!(report.added, 5);
}

// --- First reconcile ---

#[test]
fn first_reconcile_embeds_every_default_namespace_concept_and_records_model() {
    let (_local, _imported, ctx) = fixture();
    let mut index = fresh_index();

    let report = index.reconcile(&ctx).unwrap();

    // local: arch + deploy + gotchas + case; vendor: locking (its
    // memory/ is skipped by the default walk).
    assert_eq!(report.upserted, 5);
    assert_eq!(report.removed, 0);
    assert_eq!(report.unchanged, 0);
    assert!(!report.rebuilt);
    assert_eq!(report.model_id, "mock-embedder@1");

    let store = index.store();
    assert_eq!(store.model_id(), Some("mock-embedder@1"));
    let hashes = store.unit_hashes().unwrap();
    assert_eq!(hashes.len(), 5);
    assert!(
        hashes
            .keys()
            .any(|q| q.argosy == "local" && q.namespace == Namespace::Memory)
    );
    assert!(
        !hashes
            .keys()
            .any(|q| q.argosy == "vendor" && q.namespace == Namespace::Memory)
    );
}

// --- Unchanged reconcile ---

#[test]
fn second_reconcile_with_no_changes_costs_only_hashing() {
    let (_local, _imported, ctx) = fixture();
    let mut index = fresh_index();
    index.reconcile(&ctx).unwrap();
    let embed_calls_before = index.provider().embed_calls();

    let report = index.reconcile(&ctx).unwrap();

    assert_eq!(report.upserted, 0);
    assert_eq!(report.removed, 0);
    assert_eq!(report.unchanged, 5, "nothing changed ⇒ nothing re-embedded");
    assert!(!report.rebuilt);
    assert_eq!(
        index.provider().embed_calls(),
        embed_calls_before,
        "NFR-4: an unchanged concept costs one hash, never an embed"
    );
}

// --- Incremental edit + delete ---

#[test]
fn editing_one_concept_upserts_exactly_it_and_deleting_removes_it() {
    let (local, _imported, ctx) = fixture();
    let mut index = fresh_index();
    index.reconcile(&ctx).unwrap();

    // Edit the memory note in place through the write surface.
    let edited = Concept::from_str(
        "---\ntype: Note\ndescription: Build gotchas.\ntags: [build]\n---\nCargo needs a workspace.\n",
    )
    .unwrap();
    ctx.local()
        .write_concept(Namespace::Memory, &concept_id("memory/gotchas"), &edited)
        .unwrap();
    let old_hashes = index.store().unit_hashes().unwrap();

    let report = index.reconcile(&ctx).unwrap();
    assert_eq!(
        report.upserted, 1,
        "IDX-11: only the changed concept is stale"
    );
    assert_eq!(report.unchanged, 4);
    assert_eq!(report.removed, 0);
    let qid = QualifiedConceptId {
        argosy: "local".to_string(),
        namespace: Namespace::Memory,
        id: concept_id("memory/gotchas"),
    };
    let new_hashes = index.store().unit_hashes().unwrap();
    assert_ne!(old_hashes[&qid], new_hashes[&qid]);

    // Delete it: the next reconcile calls remove_concept, and search can
    // no longer return it.
    ctx.local()
        .delete_concept(Namespace::Memory, &concept_id("memory/gotchas"))
        .unwrap();
    let report = index.reconcile(&ctx).unwrap();
    assert_eq!(report.removed, 1, "deletion is incremental");
    assert_eq!(report.upserted, 0);
    assert_eq!(index.store().removals, vec![qid.clone()]);
    let hits = index
        .search(&ctx, &Query::unscoped("Cargo lockfile build gotchas", 10))
        .unwrap();
    assert!(
        hits.iter().all(|h| h.concept != qid),
        "a deleted concept must disappear from search"
    );

    let _ = local;
}

// --- Model flip ---

#[test]
fn flipping_the_model_id_clears_and_fully_rebuilds() {
    let (_local, _imported, ctx) = fixture();
    let mut index = fresh_index();
    index.reconcile(&ctx).unwrap();
    let before = index.store().unit_hashes().unwrap();

    index.set_provider(MockEmbedder::with_model_id("mock-embedder@2"));
    let report = index.reconcile(&ctx).unwrap();

    assert!(
        report.rebuilt,
        "IDX-12: identity mismatch stales the whole index"
    );
    assert_eq!(report.upserted, 5);
    assert_eq!(report.unchanged, 0);
    assert_eq!(report.removed, 0);
    assert_eq!(report.model_id, "mock-embedder@2");
    assert_eq!(index.store().model_id(), Some("mock-embedder@2"));
    assert_eq!(index.store().clears, 1, "clear happened before re-embed");

    // The same concepts are re-stored under the new identity and are
    // searchable — the cleared old vectors are gone entirely (interleaving
    // old and new models is impossible by construction).
    assert!(index.store().unit_hashes().unwrap() == before);
    let hits = index
        .search(&ctx, &Query::unscoped("architecture", 10))
        .unwrap();
    assert!(!hits.is_empty());
}

#[test]
fn rebuild_re_records_identity_even_if_clear_keeps_the_old_one() {
    // Regression for the mismatch guard previously relying on `clear()`
    // dropping the identity: a store that keeps it must still end the
    // reconcile recording the new model, or every later reconcile would
    // see a mismatch and clear + re-embed the whole corpus again.
    /// A `MemStore` whose `clear()` drops units but keeps `model_id`.
    struct StickyClear(MemStore);

    impl VectorStore for StickyClear {
        fn model_id(&self) -> Option<&str> {
            self.0.model_id()
        }
        fn set_model_id(&mut self, id: &str) -> Result<()> {
            self.0.set_model_id(id)
        }
        fn upsert(&mut self, units: &[EmbeddingUnit]) -> Result<()> {
            self.0.upsert(units)
        }
        fn remove_concept(&mut self, concept: &QualifiedConceptId) -> Result<()> {
            self.0.remove_concept(concept)
        }
        fn unit_hashes(&self) -> Result<HashMap<QualifiedConceptId, String>> {
            self.0.unit_hashes()
        }
        fn clear(&mut self) -> Result<()> {
            let keep = self.0.model_id.clone();
            self.0.units.clear();
            self.0.model_id = keep;
            Ok(())
        }
        fn search(&self, vector: &[f32], k: usize, filter: &Filter) -> Result<Vec<SearchHit>> {
            self.0.search(vector, k, filter)
        }
    }

    let (_local, _imported, ctx) = fixture();
    let mut index = Index::new(MockEmbedder::new(), StickyClear(MemStore::new()));
    index.reconcile(&ctx).unwrap();

    index.set_provider(MockEmbedder::with_model_id("mock-embedder@2"));
    let report = index.reconcile(&ctx).unwrap();
    assert!(report.rebuilt);
    assert_eq!(index.store().model_id(), Some("mock-embedder@2"));

    // The next reconcile is incremental, not another full rebuild.
    let report = index.reconcile(&ctx).unwrap();
    assert!(!report.rebuilt);
    assert_eq!(report.upserted, 0);
    assert_eq!(report.unchanged, 5);
}
