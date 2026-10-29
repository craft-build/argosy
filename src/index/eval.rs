//! The labeled retrieval eval: a JSON dataset of queries mapped to expected
//! `argosy://` URIs, scored with recall@k and MRR ([`run_eval`]).
//!
//! Scoring runs against the production [`Index::search`] path, so the eval
//! measures whatever embedding model, chunking budget, and search mode the
//! project is configured with.

use std::collections::{HashMap, HashSet};
use std::fmt;

use serde::de;
use serde::{Deserialize, Serialize};

use super::{Index, Query, SearchHit};
use crate::context::QualifiedConceptId;
use crate::error::{Result, ValidationSnafu};

/// Float comparison slack for threshold checks: a macro-average of many exact
/// reciprocals can land a hair below a floor written from the same number.
const CHECK_EPSILON: f32 = 1e-6;

/// Minimum metric values the eval must meet; unset metrics are unchecked.
///
/// Serialized as a map of `"recall@<k>"` and `"mrr"` floors, e.g.
/// `{"recall@10": 0.85, "mrr": 0.75}`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Thresholds {
    /// `(k, floor)` recall floors, sorted ascending by `k`.
    recall: Vec<(usize, f32)>,
    /// The mean-reciprocal-rank floor.
    mrr: Option<f32>,
}

impl Thresholds {
    /// An empty set: no metric is floored, so only `absent` violations can fail.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds or replaces the recall floor at `k`.
    pub fn with_recall(&mut self, k: usize, floor: f32) -> &mut Self {
        match self.recall.iter_mut().find(|(existing, _)| *existing == k) {
            Some((_, value)) => *value = floor,
            None => self.recall.push((k, floor)),
        }
        self.recall.sort_by_key(|(k, _)| *k);
        self
    }

    /// Sets the MRR floor.
    pub fn with_mrr(&mut self, floor: f32) -> &mut Self {
        self.mrr = Some(floor);
        self
    }

    /// The recall floors, ascending by `k`.
    pub fn recall_floors(&self) -> &[(usize, f32)] {
        &self.recall
    }

    /// The MRR floor, when set.
    pub fn mrr_floor(&self) -> Option<f32> {
        self.mrr
    }

    /// Whether no floor is set.
    pub fn is_empty(&self) -> bool {
        self.recall.is_empty() && self.mrr.is_none()
    }
}

impl Serialize for Thresholds {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeMap;
        let len = self.recall.len() + usize::from(self.mrr.is_some());
        let mut map = serializer.serialize_map(Some(len))?;
        for (k, floor) in &self.recall {
            map.serialize_entry(&format!("recall@{k}"), floor)?;
        }
        if let Some(mrr) = self.mrr {
            map.serialize_entry("mrr", &mrr)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for Thresholds {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct Visitor;

        impl<'de> de::Visitor<'de> for Visitor {
            type Value = Thresholds;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(
                    "a map of `recall@<k>` and/or `mrr` floors, \
                     e.g. `{\"recall@10\": 0.85, \"mrr\": 0.75}`",
                )
            }

            fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
            where
                A: de::MapAccess<'de>,
            {
                let bad_key = |key: &str| {
                    de::Error::custom(format!(
                        "unrecognized threshold key `{key}`: expected `recall@<k>` or `mrr`"
                    ))
                };
                let mut recall = Vec::new();
                let mut mrr = None;
                while let Some((key, value)) = map.next_entry::<String, f32>()? {
                    if key == "mrr" {
                        if mrr.replace(value).is_some() {
                            return Err(de::Error::custom("duplicate `mrr` floor"));
                        }
                    } else if let Some(k) = key.strip_prefix("recall@") {
                        let k = k.parse::<usize>().map_err(|_| {
                            de::Error::custom(format!(
                                "threshold key `{key}`: `recall@<k>` needs an integer k"
                            ))
                        })?;
                        recall.push((k, value));
                    } else {
                        return Err(bad_key(&key));
                    }
                }
                recall.sort_by_key(|(k, _)| *k);
                if recall.windows(2).any(|pair| pair[0].0 == pair[1].0) {
                    return Err(de::Error::custom("duplicate recall threshold for one k"));
                }
                Ok(Thresholds { recall, mrr })
            }
        }

        deserializer.deserialize_map(Visitor)
    }
}

/// One labeled query: the `expected` URIs must rank, the `absent` URIs must not.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvalQuery {
    /// The natural-language query text.
    pub query: String,
    /// `argosy://` URIs that must appear in the top-k unique-concept results.
    pub expected: Vec<String>,
    /// `argosy://` URIs that must stay out of the top-k window: the
    /// stale/deleted-content challenge entries, pointing at confusable
    /// corpus content a drifted index would surface.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub absent: Vec<String>,
}

/// A retrieval eval dataset (`evals/retrieval.json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvalDataset {
    /// The `k` cutoffs recall is reported and floored at.
    pub k: Vec<usize>,
    /// Minimum aggregate scores; the eval fails below any floor.
    #[serde(default, skip_serializing_if = "Thresholds::is_empty")]
    pub min: Thresholds,
    /// The top-k window `absent` URIs must stay out of (default: the largest
    /// `k`). Set it near the results a caller actually sees — on a small
    /// corpus, the largest `k` can span most of the book.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub absent_window: Option<usize>,
    /// The labeled queries.
    pub queries: Vec<EvalQuery>,
}

impl EvalDataset {
    /// Rejects datasets whose shape or labels cannot produce honest scores,
    /// naming the first problem found.
    ///
    /// Label URIs must parse here; existence in the built index is checked by
    /// [`run_eval`], which has the store.
    pub fn validate(&self) -> Result<()> {
        let reject = |reason: String| ValidationSnafu { reason }.fail();
        if self.k.is_empty() {
            return reject("eval dataset: `k` must list at least one cutoff".to_string());
        }
        if self.k.contains(&0) {
            return reject("eval dataset: every `k` must be >= 1".to_string());
        }
        let mut seen_k = HashSet::new();
        for k in &self.k {
            if !seen_k.insert(k) {
                return reject(format!("eval dataset: `k` lists {k} twice"));
            }
        }
        for floor in self.min.recall_floors() {
            if !self.k.contains(&floor.0) {
                return reject(format!(
                    "eval dataset: recall floor at k={} but `k` = {:?}; add the cutoff or move the floor",
                    floor.0, self.k
                ));
            }
        }
        let bad_floor = self
            .min
            .recall_floors()
            .iter()
            .map(|(_, floor)| *floor)
            .chain(self.min.mrr_floor())
            .find(|floor| !(0.0..=1.0).contains(floor));
        if let Some(floor) = bad_floor {
            return reject(format!(
                "eval dataset: floors must be within 0–1, got {floor}"
            ));
        }
        if self.queries.is_empty() {
            return reject("eval dataset: `queries` must hold at least one entry".to_string());
        }
        if let Some(window) = self.absent_window
            && window == 0
        {
            return reject("eval dataset: `absent_window` must be >= 1".to_string());
        }
        for (index, query) in self.queries.iter().enumerate() {
            if query.query.trim().is_empty() {
                return reject(format!("eval dataset: query {index} has empty text"));
            }
            if query.expected.is_empty() {
                return reject(format!(
                    "eval dataset: query {index} ({:?}) lists no `expected` URIs",
                    query.query
                ));
            }
            let mut labels: HashSet<&str> = HashSet::new();
            let absent: HashSet<&str> = query.absent.iter().map(String::as_str).collect();
            for uri in query.expected.iter().chain(&query.absent) {
                if QualifiedConceptId::from_uri(uri).is_err() {
                    return reject(format!(
                        "eval dataset: query {index} ({:?}) has unparseable label URI `{uri}`",
                        query.query
                    ));
                }
                if absent.contains(uri.as_str()) && labels.contains(uri.as_str()) {
                    return reject(format!(
                        "eval dataset: query {index} ({:?}) lists `{uri}` as both expected and absent",
                        query.query
                    ));
                }
                if !labels.insert(uri.as_str()) {
                    return reject(format!(
                        "eval dataset: query {index} ({:?}) lists `{uri}` twice",
                        query.query
                    ));
                }
            }
        }
        Ok(())
    }
}

/// One labeled query's scored outcome.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QueryScore {
    /// The query text, echoed for report readability.
    pub query: String,
    /// The 1-based rank of each `expected` URI over unique concepts;
    /// `None` means it was not retrieved.
    pub ranks: Vec<(String, Option<usize>)>,
    /// Recall at each dataset `k`, in dataset order.
    pub recall_at_k: Vec<(usize, f32)>,
    /// One over the best expected rank; `0.0` when nothing expected ranked.
    pub reciprocal_rank: f32,
    /// `absent` URIs that surfaced inside the window, with the rank they
    /// surfaced at (challenge failures).
    pub absent_violations: Vec<(String, usize)>,
}

/// One threshold (or `absent`-challenge) check in the report.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ThresholdCheck {
    /// The metric as printed: `recall@<k>`, `mrr`, or `absent-violations`.
    pub metric: String,
    /// The value the eval required.
    pub required: f32,
    /// The value observed.
    pub observed: f32,
    /// Whether the observed value met the requirement.
    pub passed: bool,
}

/// The full eval outcome: per-query scores, aggregates, and the gate verdict.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EvalReport {
    /// Per-query outcomes, in dataset order.
    pub queries: Vec<QueryScore>,
    /// Macro-averaged recall at each dataset `k`, in dataset order.
    pub recall_at_k: Vec<(usize, f32)>,
    /// The mean reciprocal rank over all queries.
    pub mrr: f32,
    /// Every applied threshold check, plus the `absent`-violation check when
    /// the dataset carries challenge labels.
    pub checks: Vec<ThresholdCheck>,
    /// Whether every check passed — the gate CI exits on.
    pub passed: bool,
}

/// Collapses passage hits to their concepts' unique ranking: the first (best)
/// position of each `argosy://` URI, so one long document cannot crowd the top-k.
fn unique_concept_ranks(hits: &[SearchHit]) -> Vec<String> {
    let mut seen = HashSet::new();
    hits.iter()
        .map(|hit| hit.concept.to_uri())
        .filter(|uri| seen.insert(uri.clone()))
        .collect()
}

/// Scores one query against its fused hits: ranks over unique concepts,
/// recall at each `k`, reciprocal rank, and `absent` violations.
///
/// `absent_window` is the rank ceiling an `absent` URI must stay beyond.
pub(crate) fn score_query(
    query: &EvalQuery,
    hits: &[SearchHit],
    ks: &[usize],
    absent_window: usize,
) -> QueryScore {
    let ranking = unique_concept_ranks(hits);
    let positions: HashMap<&str, usize> = ranking
        .iter()
        .enumerate()
        .map(|(index, uri)| (uri.as_str(), index + 1))
        .collect();
    let ranks = query
        .expected
        .iter()
        .map(|uri| (uri.clone(), positions.get(uri.as_str()).copied()))
        .collect();
    let recall_at_k = ks
        .iter()
        .map(|k| {
            let found = query
                .expected
                .iter()
                .filter(|uri| positions.get(uri.as_str()).is_some_and(|rank| rank <= k))
                .count();
            (*k, found as f32 / query.expected.len() as f32)
        })
        .collect();
    let reciprocal_rank = query
        .expected
        .iter()
        .filter_map(|uri| positions.get(uri.as_str()))
        .min()
        .map_or(0.0, |rank| 1.0 / *rank as f32);
    let absent_violations = query
        .absent
        .iter()
        .filter_map(|uri| {
            positions
                .get(uri.as_str())
                .filter(|rank| **rank <= absent_window)
                .map(|rank| (uri.clone(), *rank))
        })
        .collect();
    QueryScore {
        query: query.query.clone(),
        ranks,
        recall_at_k,
        reciprocal_rank,
        absent_violations,
    }
}

/// Runs every dataset query against the index and scores the fused hits.
/// A label naming a concept the built index does not hold fails loudly (a
/// typo'd label must never silently score zero; see [`EvalDataset::validate`]).
///
/// Each query searches with an over-fetch depth (the same `4×k, min 32` the
/// hybrid legs use) because [`Index::search`] truncates the fused list before
/// this eval collapses passages to concepts.
pub fn run_eval<P: super::EmbeddingProvider, S: super::VectorStore>(
    index: &Index<P, S>,
    context: &crate::context::ProjectContext,
    dataset: &EvalDataset,
) -> Result<EvalReport> {
    dataset.validate()?;

    // Label hygiene: every expected and absent URI must be in the built book.
    let indexed: HashSet<String> = index
        .store()
        .unit_hashes()?
        .keys()
        .map(|qid| qid.to_uri())
        .collect();
    let mut unknown: Vec<String> = Vec::new();
    for query in &dataset.queries {
        for uri in query.expected.iter().chain(&query.absent) {
            if !indexed.contains(uri) && !unknown.contains(uri) {
                unknown.push(uri.clone());
            }
        }
    }
    if !unknown.is_empty() {
        return ValidationSnafu {
            reason: format!(
                "eval dataset labels not present in the index: {}. Labels must name \
                 concepts in the built book (is the index built and does the manifest \
                 name match?)",
                unknown.join(", ")
            ),
        }
        .fail();
    }

    let ks = dataset.k.clone();
    let max_k = *ks
        .iter()
        .max()
        .expect("validate guaranteed a non-empty k list");
    let absent_window = dataset.absent_window.unwrap_or(max_k);
    let fetch = super::hybrid_fetch_k(max_k);
    let mut queries = Vec::with_capacity(dataset.queries.len());
    for query in &dataset.queries {
        let hits = index.search(context, &Query::unscoped(&query.query, fetch))?;
        queries.push(score_query(query, &hits, &ks, absent_window));
    }

    let recall_at_k: Vec<(usize, f32)> = ks
        .iter()
        .map(|k| {
            let mean = queries
                .iter()
                .map(|score| {
                    score
                        .recall_at_k
                        .iter()
                        .find(|(query_k, _)| query_k == k)
                        .map_or(0.0, |(_, recall)| *recall)
                })
                .sum::<f32>()
                / queries.len() as f32;
            (*k, mean)
        })
        .collect();
    let mrr = queries
        .iter()
        .map(|score| score.reciprocal_rank)
        .sum::<f32>()
        / queries.len() as f32;

    let mut checks = Vec::new();
    for (k, floor) in dataset.min.recall_floors() {
        let observed = recall_at_k
            .iter()
            .find(|(query_k, _)| query_k == k)
            .map_or(0.0, |(_, recall)| *recall);
        checks.push(ThresholdCheck {
            metric: format!("recall@{k}"),
            required: *floor,
            observed,
            passed: observed + CHECK_EPSILON >= *floor,
        });
    }
    if let Some(floor) = dataset.min.mrr_floor() {
        checks.push(ThresholdCheck {
            metric: "mrr".to_string(),
            required: floor,
            observed: mrr,
            passed: mrr + CHECK_EPSILON >= floor,
        });
    }
    let violations: usize = dataset
        .queries
        .iter()
        .map(|query| query.absent.len())
        .sum::<usize>();
    if violations > 0 {
        let observed = queries
            .iter()
            .map(|score| score.absent_violations.len())
            .sum::<usize>() as f32;
        checks.push(ThresholdCheck {
            metric: "absent-violations".to_string(),
            required: 0.0,
            observed,
            passed: observed == 0.0,
        });
    }

    let passed = checks.iter().all(|check| check.passed);
    Ok(EvalReport {
        queries,
        recall_at_k,
        mrr,
        checks,
        passed,
    })
}
