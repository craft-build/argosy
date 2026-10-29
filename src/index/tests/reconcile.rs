//! Reconcile lifecycle tests: first run, unchanged run, incremental
//! edits, deletes, and model flips.

use super::*;

// --- staleness_report: the read-only preview must mirror reconcile ---

#[test]
fn staleness_after_reconcile_is_all_unchanged() {
    let (_local, _imported, ctx) = fixture();
    let mut index = fresh_index();
    index.reconcile(&ctx).unwrap();

    let report = staleness_report(
        &ctx,
        index.store(),
        "mock-embedder@1",
        ChunkConfig::default(),
    )
    .unwrap();

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

    write_file(local.path(), "memory/gotchas.md", "Edited build notes.\n");
    fs::remove_file(local.path().join("skill/deploy.md")).unwrap();
    write_file(
        local.path(),
        "document/new-note.md",
        "---\ntype: Note\n---\nA new note.\n",
    );

    let report = staleness_report(
        &ctx,
        index.store(),
        "mock-embedder@1",
        ChunkConfig::default(),
    )
    .unwrap();

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

    // A recorded identity differing from the expected model ⇒ mismatch.
    let report =
        staleness_report(&ctx, index.store(), "other-model@2", ChunkConfig::default()).unwrap();
    assert!(report.model_mismatch);

    // Reconcile's `None ⇒ !stored.is_empty()` guard: data with no identity is a rebuild.
    index.store_mut().model_id = None;
    let report = staleness_report(
        &ctx,
        index.store(),
        "mock-embedder@1",
        ChunkConfig::default(),
    )
    .unwrap();
    assert!(
        report.model_mismatch,
        "data without a recorded identity is a rebuild, never \"up to date\""
    );

    // An empty store with no recorded identity announces nothing.
    index.store_mut().clear().unwrap();
    let report = staleness_report(
        &ctx,
        index.store(),
        "mock-embedder@1",
        ChunkConfig::default(),
    )
    .unwrap();
    assert!(!report.model_mismatch);
    assert_eq!(report.added, 5);
}

// --- First reconcile ---

#[test]
fn progress_counts_committed_batches_and_only_changed_concepts() {
    let (local, _imported, ctx) = fixture();
    for n in 0..62 {
        write_file(
            local.path(),
            &format!("document/extra-{n}.md"),
            &format!("---\ntype: Note\n---\nExtra document {n}.\n"),
        );
    }
    let mut index = fresh_index();
    let mut events = Vec::new();
    let report = index
        .reconcile_with_progress(&ctx, |done, total| events.push((done, total)))
        .unwrap();
    assert_eq!(report.upserted, 67);
    assert_eq!(events, [(0, 67), (32, 67), (64, 67), (67, 67)]);
    assert_eq!(index.provider().embed_calls(), 67);
    assert_eq!(index.store().unit_hashes().unwrap().len(), 67);

    events.clear();
    index
        .reconcile_with_progress(&ctx, |done, total| events.push((done, total)))
        .unwrap();
    assert_eq!(events, [(0, 0)]);
    assert_eq!(index.provider().embed_calls(), 67);

    write_file(local.path(), "document/extra-0.md", "Changed document.\n");
    events.clear();
    index
        .reconcile_with_progress(&ctx, |done, total| events.push((done, total)))
        .unwrap();
    assert_eq!(events, [(0, 1), (1, 1)]);
    assert_eq!(index.provider().embed_calls(), 68);

    index.set_provider(MockEmbedder::with_model_id("mock-embedder@2"));
    events.clear();
    let report = index
        .reconcile_with_progress(&ctx, |done, total| events.push((done, total)))
        .unwrap();
    assert!(report.rebuilt);
    assert_eq!(events, [(0, 67), (32, 67), (64, 67), (67, 67)]);
}

#[test]
fn interrupted_incremental_build_reports_only_committed_work_and_resumes() {
    struct FailingEmbedder {
        inner: MockEmbedder,
        calls: Cell<usize>,
        fail_on: Cell<usize>,
    }

    impl EmbeddingProvider for FailingEmbedder {
        fn model_id(&self) -> &str {
            self.inner.model_id()
        }

        fn dimensions(&self) -> usize {
            self.inner.dimensions()
        }

        fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            let call = self.calls.get() + 1;
            self.calls.set(call);
            if call == self.fail_on.get() {
                return Err(Error::Index {
                    reason: "simulated embedding failure".to_string(),
                });
            }
            self.inner.embed(texts)
        }
    }

    let (local, _imported, ctx) = fixture();
    let mut index = Index::new(
        FailingEmbedder {
            inner: MockEmbedder::new(),
            calls: Cell::new(0),
            fail_on: Cell::new(usize::MAX),
        },
        MemStore::new(),
    );
    index.reconcile(&ctx).unwrap();
    for n in 0..35 {
        write_file(
            local.path(),
            &format!("document/new-{n}.md"),
            &format!("---\ntype: Note\n---\nNew document {n}.\n"),
        );
    }
    index.provider().fail_on.set(3);
    let mut events = Vec::new();
    assert!(
        index
            .reconcile_with_progress(&ctx, |done, total| events.push((done, total)))
            .is_err()
    );
    assert_eq!(events, [(0, 35), (32, 35)]);
    assert_eq!(index.store().unit_hashes().unwrap().len(), 37);

    index.provider().fail_on.set(usize::MAX);
    events.clear();
    let report = index
        .reconcile_with_progress(&ctx, |done, total| events.push((done, total)))
        .unwrap();
    assert_eq!(report.upserted, 3);
    assert_eq!(events, [(0, 3), (3, 3)]);
    assert_eq!(index.store().unit_hashes().unwrap().len(), 40);
}

#[test]
fn first_reconcile_embeds_every_default_namespace_concept_and_records_model() {
    let (_local, _imported, ctx) = fixture();
    let mut index = fresh_index();

    let report = index.reconcile(&ctx).unwrap();

    // Four local concepts plus vendor's locking; its memory/ is skipped by the default walk.
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

    // Old and new vectors never interleave: everything is re-stored under the new identity.
    assert!(index.store().unit_hashes().unwrap() == before);
    let hits = index
        .search(&ctx, &Query::unscoped("architecture", 10))
        .unwrap();
    assert!(!hits.is_empty());
}

#[test]
fn rebuild_re_records_identity_even_if_clear_keeps_the_old_one() {
    // Without re-recording, every later reconcile would be another full rebuild.
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

    let report = index.reconcile(&ctx).unwrap();
    assert!(!report.rebuilt);
    assert_eq!(report.upserted, 0);
    assert_eq!(report.unchanged, 5);
}

// --- Multi-chunk concepts ---

/// A concept long enough to split under the default budget.
fn long_concept() -> String {
    let filler = "word ".repeat(90);
    format!(
        "---\ntype: Note\ndescription: A long architecture note.\n---\n\
         # Architecture\n\nIntro prose.\n\n\
         ## Ingestion\n\n{filler}\n\n\
         ### Retries\n\n{filler}\n\n\
         ## Storage\n\n{filler}\n"
    )
}

#[test]
fn a_long_concept_upserts_one_unit_per_passage_and_counts_them() {
    let (local, _imported, ctx) = fixture();
    write_file(local.path(), "document/long.md", &long_concept());
    let mut index = fresh_index();

    let report = index.reconcile(&ctx).unwrap();
    assert_eq!(
        report.upserted, 6,
        "five fixture concepts plus the long one"
    );
    assert!(
        report.passages > report.upserted,
        "the long concept expanded into several passages: {report:?}"
    );
    // The concept-level diff still sees one entry per concept.
    assert_eq!(index.store().unit_hashes().unwrap().len(), 6);
}

#[test]
fn a_long_concept_is_retrievable_past_its_first_paragraph() {
    let (local, _imported, ctx) = fixture();
    write_file(
        local.path(),
        "document/long.md",
        &format!(
            "---\ntype: Note\n---\n# Architecture\n\nIntro.\n\n## Early\n\n{}\n\n## Late Section\n\n\
             quasar telemetry handshake protocol\n",
            "word ".repeat(200)
        ),
    );
    let mut index = fresh_index();
    index.reconcile(&ctx).unwrap();

    let hits = index
        .search(&ctx, &Query::unscoped("quasar telemetry handshake", 5))
        .unwrap();
    let top = hits.first().expect("the tail passage is retrievable");
    assert_eq!(top.concept.id.as_str(), "document/long");
    assert!(
        top.passage.contains("quasar telemetry"),
        "the hit is the passage, not the concept head: {}",
        top.passage
    );
    assert_eq!(top.heading_path, "Architecture › Late Section");
    assert!(top.start_line > 1);
    assert!(top.end_line >= top.start_line);
}

#[test]
fn editing_one_section_reembeds_only_the_concept_holding_it() {
    let (local, _imported, ctx) = fixture();
    write_file(local.path(), "document/long.md", &long_concept());
    let mut index = fresh_index();
    index.reconcile(&ctx).unwrap();
    let embedded = index.provider().embed_calls();

    // Same length, different words — the concept hash must still change.
    let edited = long_concept().replace("## Storage", "## Persistence");
    write_file(local.path(), "document/long.md", &edited);
    let report = index.reconcile(&ctx).unwrap();

    assert_eq!(report.upserted, 1, "IDX-11 holds at concept granularity");
    assert_eq!(report.unchanged, 5);
    assert!(
        index.provider().embed_calls() > embedded,
        "the changed concept re-embedded its passages"
    );
}

#[test]
fn deleting_a_long_concept_removes_every_passage() {
    let (local, _imported, ctx) = fixture();
    write_file(local.path(), "document/long.md", &long_concept());
    let mut index = fresh_index();
    index.reconcile(&ctx).unwrap();
    let qid = QualifiedConceptId {
        argosy: "local".to_string(),
        namespace: Namespace::Document,
        id: concept_id("document/long"),
    };

    fs::remove_file(local.path().join("document/long.md")).unwrap();
    let report = index.reconcile(&ctx).unwrap();
    assert_eq!(report.removed, 1);
    let hits = index.search(&ctx, &Query::unscoped("quasar", 10)).unwrap();
    assert!(hits.iter().all(|h| h.concept != qid), "no orphan passage");
}
