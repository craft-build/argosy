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
