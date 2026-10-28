//! `ask`: embedding retrieval augmented by the decision endpoint.
#![cfg(feature = "decision")]

use std::sync::Arc;

use super::*;
use crate::decision::{DecisionProvider, DecisionRequest, DecisionResponse};
use crate::error::{Error, Result};

/// A decision double: returns a fixed `choice` (or a failure).
struct FakeDecision {
    choice: Option<&'static str>,
    confidence: f32,
    fail: bool,
}

impl DecisionProvider for FakeDecision {
    fn is_enabled(&self) -> bool {
        true
    }

    fn decide(&self, _request: &DecisionRequest) -> Result<DecisionResponse> {
        if self.fail {
            return Err(Error::Decision {
                reason: "boom".to_string(),
            });
        }
        let mut answer = serde_json::json!({ "answer_confidence": self.confidence });
        if let Some(choice) = self.choice {
            answer["choice"] = serde_json::json!(choice);
        }
        Ok(serde_json::from_value(serde_json::json!({ "answers": { "answer": answer } })).unwrap())
    }
}

fn ask_params(query: &str) -> AskParams {
    AskParams {
        cwd: project(),
        query: query.to_string(),
        k: Some(4),
    }
}

#[test]
fn ask_with_decisions_disabled_returns_embedding_hits() {
    let mut rig = rig();
    let report = rig
        .state
        .ask(ask_params("rate limit retries original request timestamp"))
        .unwrap();
    assert_eq!(report.mode, "search");
    assert!(report.selected.is_none());
    assert!(report.note.is_some());
    assert!(!report.candidates.is_empty(), "candidates still returned");
}

#[test]
fn ask_selects_a_candidate_when_the_endpoint_chooses_one() {
    let rig = rig();
    let mut state = rig.state.with_decision(Arc::new(FakeDecision {
        choice: Some("A"),
        confidence: 0.93,
        fail: false,
    }));
    let report = state
        .ask(ask_params("rate limit retries original request timestamp"))
        .unwrap();
    assert_eq!(report.mode, "selected");
    let selected = report.selected.expect("selection present");
    assert_eq!(selected.label, "A");
    assert_eq!(
        selected.uri, report.candidates[0].uri,
        "label A is the top candidate"
    );
    assert_eq!(selected.answer_confidence, Some(0.93));
    assert!(report.note.is_none());
}

#[test]
fn ask_treats_none_as_search_only() {
    let rig = rig();
    let mut state = rig.state.with_decision(Arc::new(FakeDecision {
        choice: Some("NONE"),
        confidence: 0.5,
        fail: false,
    }));
    let report = state.ask(ask_params("an unrelated question")).unwrap();
    assert_eq!(report.mode, "search");
    assert!(report.selected.is_none());
    assert!(
        report
            .note
            .as_deref()
            .unwrap_or_default()
            .contains("no document"),
        "note explains the none outcome: {:?}",
        report.note
    );
}

#[test]
fn ask_fails_open_when_the_endpoint_errors() {
    let rig = rig();
    let mut state = rig.state.with_decision(Arc::new(FakeDecision {
        choice: None,
        confidence: 0.0,
        fail: true,
    }));
    let report = state.ask(ask_params("rate limit retries")).unwrap();
    assert_eq!(report.mode, "search");
    assert!(report.selected.is_none());
    assert!(
        report
            .note
            .as_deref()
            .unwrap_or_default()
            .contains("unavailable"),
        "note explains the degradation: {:?}",
        report.note
    );
    assert!(!report.candidates.is_empty());
}

#[test]
fn ask_candidates_collapse_passages_to_one_per_concept() {
    // A long document splits into many passages; without collapsing, one
    // document could fill the whole candidate list and the decision
    // endpoint (which picks a *document*) would see duplicate options.
    let rig = rig();
    let root = rig._local.path().to_path_buf();
    let filler = "quasar telemetry handshake protocol ".repeat(60);
    let long = format!(
        "---\ntype: Note\ndescription: Long protocol note.\n---\n\
         # Protocol\n\n{filler}\n\n## Handshake\n\n{filler}\n\n## Telemetry\n\n{filler}\n"
    );
    std::fs::write(root.join("document/long-protocol.md"), long).unwrap();

    let mut state = McpState::new({
        let root = root.clone();
        Arc::new(move |_root| {
            let context = ProjectContext::open(&root, [])?;
            let mut index = Index::new(MockEmbedder::new(), MemStore::new());
            index.reconcile(&context)?;
            Ok(ProjectSession::new(context, index))
        })
    });
    let report = state
        .ask(AskParams {
            cwd: root.clone(),
            query: "quasar telemetry handshake protocol".to_string(),
            k: Some(8),
        })
        .unwrap();
    assert!(!report.candidates.is_empty());
    let mut uris: Vec<&str> = report.candidates.iter().map(|c| c.uri.as_str()).collect();
    let total = uris.len();
    uris.sort();
    uris.dedup();
    assert_eq!(
        uris.len(),
        total,
        "one candidate per concept, no duplicate passages: {:?}",
        report.candidates.iter().map(|c| &c.uri).collect::<Vec<_>>()
    );
    let top = &report.candidates[0];
    assert!(
        !top.passage.is_empty(),
        "candidates carry their matched passage"
    );
}
