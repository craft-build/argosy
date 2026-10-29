//! Eval scoring semantics: thresholds parsing, dataset validation, and the
//! recall@k / MRR math over both synthetic hits and the fixture book.

use super::*;
use crate::index::eval::{run_eval, score_query};

// --- Thresholds parsing ---

#[test]
fn thresholds_round_trip_through_the_map_form() {
    let raw = r#"{"recall@1": 0.5, "recall@10": 0.85, "mrr": 0.75}"#;
    let parsed: Thresholds = serde_json::from_str(raw).unwrap();
    assert_eq!(parsed.recall_floors(), &[(1, 0.5), (10, 0.85)]);
    assert_eq!(parsed.mrr_floor(), Some(0.75));
    // Exact key order is part of the serialization: floors ascending, then mrr.
    assert_eq!(
        serde_json::to_string(&parsed).unwrap(),
        r#"{"recall@1":0.5,"recall@10":0.85,"mrr":0.75}"#
    );
    assert!(Thresholds::default().is_empty());
}

#[test]
fn thresholds_reject_unknown_or_malformed_keys() {
    for raw in [
        r#"{"recall": 0.5}"#,
        r#"{"recall@ten": 0.5}"#,
        r#"{"mrr@5": 0.5}"#,
        r#"{"recall@5": 0.5, "recall@5": 0.6}"#,
    ] {
        let err = serde_json::from_str::<Thresholds>(raw)
            .expect_err("malformed thresholds must not parse");
        assert!(err.to_string().contains("threshold"), "{raw}: {err}");
    }
}

// --- Dataset validation ---

/// A minimal dataset that passes [`EvalDataset::validate`].
fn valid_dataset() -> EvalDataset {
    let mut dataset: EvalDataset = serde_json::from_str(
        r#"{
            "k": [5],
            "queries": [
                {"query": "the architecture", "expected": ["argosy://local/document/arch"]}
            ]
        }"#,
    )
    .unwrap();
    let mut min = Thresholds::new();
    min.with_recall(5, 0.5);
    dataset.min = min;
    dataset
}

#[test]
fn dataset_validation_accepts_the_minimal_shape() {
    valid_dataset().validate().unwrap();
}

#[test]
fn dataset_validation_rejects_shape_mistakes_with_actionable_messages() {
    let cases: Vec<(EvalDataset, &str)> = vec![
        {
            let mut ds = valid_dataset();
            ds.k = vec![];
            (ds, "at least one cutoff")
        },
        {
            let mut ds = valid_dataset();
            ds.k = vec![0];
            (ds, ">= 1")
        },
        {
            let mut ds = valid_dataset();
            ds.k = vec![5, 5];
            (ds, "twice")
        },
        {
            let mut ds = valid_dataset();
            let mut min = Thresholds::new();
            min.with_recall(20, 0.5);
            ds.min = min;
            (ds, "add the cutoff")
        },
        {
            let mut ds = valid_dataset();
            let mut min = Thresholds::new();
            min.with_mrr(1.5);
            ds.min = min;
            (ds, "0–1")
        },
        {
            let mut ds = valid_dataset();
            ds.queries = vec![];
            (ds, "at least one entry")
        },
        {
            let mut ds = valid_dataset();
            ds.queries[0].expected = vec![];
            (ds, "no `expected` URIs")
        },
        {
            let mut ds = valid_dataset();
            ds.queries[0].query = "  ".to_string();
            (ds, "empty text")
        },
        {
            let mut ds = valid_dataset();
            ds.queries[0].expected = vec!["not-a-uri".to_string()];
            (ds, "unparseable label URI")
        },
        {
            let mut ds = valid_dataset();
            ds.queries[0].expected = vec![
                "argosy://local/document/arch".to_string(),
                "argosy://local/document/arch".to_string(),
            ];
            (ds, "twice")
        },
        {
            let mut ds = valid_dataset();
            ds.queries[0].expected = vec!["argosy://local/document/arch".to_string()];
            ds.queries[0].absent = vec!["argosy://local/document/arch".to_string()];
            (ds, "both expected and absent")
        },
    ];
    for (dataset, needle) in cases {
        let err = dataset
            .validate()
            .expect_err("invalid dataset must be rejected");
        let msg = format!("{err:#}");
        assert!(msg.contains(needle), "expected `{needle}` in `{msg}`");
    }
}

// --- Scoring over synthetic hits ---

/// A hit carrying only what scoring reads: identity and position (via score order).
fn hit(argosy: &str, id: &str, chunk_ordinal: u32, score: f32) -> SearchHit {
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

fn eval_query(expected: &[&str], absent: &[&str]) -> EvalQuery {
    EvalQuery {
        query: "synthetic".to_string(),
        expected: expected.iter().map(|s| (*s).to_string()).collect(),
        absent: absent.iter().map(|s| (*s).to_string()).collect(),
    }
}

#[test]
fn scoring_collapses_passages_into_unique_concept_ranks() {
    // Two passages of one document must count once, at its best position.
    let hits = vec![
        hit("local", "document/long", 0, 0.9),
        hit("local", "document/long", 1, 0.8),
        hit("local", "document/b", 0, 0.7),
        hit("local", "document/c", 0, 0.6),
    ];
    let query = eval_query(
        &["argosy://local/document/b", "argosy://local/document/c"],
        &[],
    );
    let score = score_query(&query, &hits, &[2, 3], 3);
    assert_eq!(
        score.ranks,
        vec![
            ("argosy://local/document/b".to_string(), Some(2)),
            ("argosy://local/document/c".to_string(), Some(3)),
        ]
    );
    let recall: HashMap<usize, f32> = score.recall_at_k.iter().copied().collect();
    assert_eq!(recall[&2], 0.5, "only b is within the top 2");
    assert_eq!(recall[&3], 1.0);
    assert_eq!(score.reciprocal_rank, 0.5, "first expected hit at rank 2");
    assert!(score.absent_violations.is_empty());
}

#[test]
fn scoring_marks_misses_and_absent_violations_within_the_window() {
    let hits = vec![
        hit("local", "document/arch", 0, 0.9),
        hit("local", "document/b", 0, 0.8),
        hit("vendor", "document/locking", 0, 0.7),
    ];
    let query = eval_query(
        &["argosy://local/document/ghost"],
        &["argosy://vendor/document/locking"],
    );
    let score = score_query(&query, &hits, &[2], 2);
    // The ghost is nowhere in the hits; the confusable ranked third, past the
    // k=2 window.
    assert_eq!(
        score.ranks,
        vec![("argosy://local/document/ghost".to_string(), None)]
    );
    assert_eq!(score.recall_at_k, vec![(2, 0.0)]);
    assert_eq!(score.reciprocal_rank, 0.0);
    assert!(
        score.absent_violations.is_empty(),
        "absent beyond the window is no violation"
    );

    // Widen the window to every hit: the confusable now violates, at its rank.
    let score = score_query(&query, &hits, &[3], 3);
    assert_eq!(
        score.absent_violations,
        vec![("argosy://vendor/document/locking".to_string(), 3)]
    );
}

// --- run_eval over the fixture book ---

#[test]
fn run_eval_scores_the_fixture_book_and_passes_its_floors() {
    let (_local, _imported, ctx) = fixture();
    let mut index = fresh_index();
    index.reconcile(&ctx).unwrap();

    let dataset = serde_json::from_str(
        r#"{
            "k": [1, 5],
            "min": {"recall@5": 1.0, "mrr": 0.5},
            "queries": [
                {"query": "service architecture design",
                 "expected": ["argosy://local/document/arch"]},
                {"query": "database locking retries",
                 "expected": ["argosy://vendor/document/locking"]},
                {"query": "deploy the service",
                 "expected": ["argosy://local/skill/deploy"]}
            ]
        }"#,
    )
    .unwrap();

    let report = run_eval(&index, &ctx, &dataset).unwrap();
    assert_eq!(report.queries.len(), 3);
    // Every expected URI is the corpus's obvious match, so all are retrieved;
    // exact first ranks are left to the aggregate floors below.
    assert!(
        report
            .queries
            .iter()
            .all(|score| score.ranks.iter().all(|(_, rank)| rank.is_some())),
        "every expected URI is retrieved: {:?}",
        report.queries.iter().map(|s| &s.ranks).collect::<Vec<_>>()
    );
    let recall: HashMap<usize, f32> = report.recall_at_k.iter().copied().collect();
    assert_eq!(
        recall[&5], 1.0,
        "a 5-concept corpus holds every URI within k=5"
    );
    assert!(recall[&1] >= 1.0 / 3.0, "recall@1 = {}", recall[&1]);
    assert!(
        report.mrr >= 0.2 && report.mrr <= 1.0,
        "mrr = {}",
        report.mrr
    );
    assert!(report.passed, "checks: {:?}", report.checks);
    assert_eq!(report.checks.len(), 2, "recall@5 and mrr, no absent labels");
}

#[test]
fn run_eval_fails_loudly_when_a_label_is_missing_from_the_index() {
    let (_local, _imported, ctx) = fixture();
    let mut index = fresh_index();
    index.reconcile(&ctx).unwrap();

    let dataset = serde_json::from_str(
        r#"{
            "k": [5],
            "queries": [
                {"query": "architecture",
                 "expected": ["argosy://local/document/ghost"]}
            ]
        }"#,
    )
    .unwrap();
    let err = run_eval(&index, &ctx, &dataset).expect_err("unknown labels must fail");
    let msg = format!("{err:#}");
    assert!(msg.contains("not present in the index"), "{msg}");
    assert!(msg.contains("argosy://local/document/ghost"), "{msg}");
}

#[test]
fn run_eval_gates_on_floors_and_absent_violations() {
    let (_local, _imported, ctx) = fixture();
    let mut index = fresh_index();
    index.reconcile(&ctx).unwrap();

    // recall@1 can never reach 1.0 with two expected URIs for one query:
    // the gate fails by arithmetic, not by embedding luck.
    let dataset = serde_json::from_str(
        r#"{
            "k": [1, 5],
            "min": {"recall@1": 1.0},
            "queries": [
                {"query": "service architecture design",
                 "expected": ["argosy://local/document/arch",
                              "argosy://vendor/document/locking"]}
            ]
        }"#,
    )
    .unwrap();
    let report = run_eval(&index, &ctx, &dataset).unwrap();
    assert!(!report.passed);
    let recall1 = report
        .checks
        .iter()
        .find(|check| check.metric == "recall@1")
        .unwrap();
    assert!(!recall1.passed);

    // An absent label that ranks inside the window is a drift-challenge failure.
    // The corpus holds 5 concepts and the fetch depth returns them all, so the
    // confusable necessarily ranks within the k=5 window.
    let dataset = serde_json::from_str(
        r#"{
            "k": [5],
            "queries": [
                {"query": "service architecture design",
                 "expected": ["argosy://local/document/arch"],
                 "absent": ["argosy://vendor/document/locking"]}
            ]
        }"#,
    )
    .unwrap();
    let report = run_eval(&index, &ctx, &dataset).unwrap();
    let check = report
        .checks
        .iter()
        .find(|check| check.metric == "absent-violations")
        .expect("absent labels always add a check");
    assert_eq!(check.observed, 1.0, "the confusable surfaced in the window");
    assert!(!check.passed);
    assert!(!report.passed);
}
