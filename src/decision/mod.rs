//! The optional decision layer: a generic client for a **Jev-compatible**
//! decision endpoint (`POST /v1/systemone`), which both the self-hosted
//! [`laya-serve`](https://huggingface.co/convaiinnovations/laya) and the
//! hosted TypeSafe Jev speak.
//!
//! This is the decision sibling of the embedding layer: a small trait
//! boundary ([`DecisionProvider`]) with a no-op [`Disabled`] default and one
//! HTTP implementation ([`JevClient`]). argosy ships no model weights and
//! takes no vendor dependency — a consumer points [`crate::config::DecisionConfig`]
//! at an endpoint it runs or subscribes to, or leaves decisions disabled.
//!
//! The wire vocabulary is typed (`choice` / `score` / `noul`) and dynamic
//! (option sets are defined per request), so requests and responses are
//! carried as JSON. The client exposes the raw per-answer fields faithfully
//! and normalizes only what every Jev-compatible server shares: the answer
//! value and, when present, `answer_confidence`. It deliberately does **not**
//! reinterpret `confidence`: Laya computes it as entropy, Jev as a
//! max-probability formula, so thresholds are the consumer's to fit per
//! endpoint.
//!
//! Calls are blocking (reqwest's blocking client) and must run off async
//! workers — argosy's MCP dispatch already routes them through
//! `spawn_blocking`.
//!
//! Requests are kept within a configured input-token estimate
//! ([`crate::config::DecisionConfig::max_input_tokens`], default 512 — the
//! smallest context laya's checkpoints read): an over-budget request is
//! trimmed before sending, longest fields first and marked in place, so a
//! long question, rule set, or diff hunk neither errors at the endpoint
//! nor gets silently cut past its questions. See the private `budget`
//! submodule for the estimate and the trimming rules.

mod budget;

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::config::{DecisionConfig, DecisionProviderConfig};
use crate::error::{Error, Result};

/// How long a failed endpoint is treated as unreachable before another
/// network attempt is made. Bounds the cost of a down sidecar on a hot path.
const DOWN_COOLDOWN: Duration = Duration::from_secs(30);

/// The `answer_confidence`-style keys a Jev-compatible answer may carry, in
/// preference order.
const CONFIDENCE_KEYS: [&str; 2] = ["answer_confidence", "confidence"];

/// The answer key for each question type, in preference order.
const ANSWER_KEYS: [&str; 3] = ["choice", "score", "noul"];

/// One typed question in the Jev `choice` / `score` / `noul` vocabulary.
/// Serializes to the wire shape (`{"type": "noul", "instructions": ...}`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Question {
    /// A single label from a named option set.
    Choice {
        instructions: String,
        criteria: BTreeMap<String, String>,
    },
    /// An ordinal level from an ordered rubric.
    Score {
        instructions: String,
        criteria: Vec<String>,
    },
    /// A calibrated yes/no probability.
    Noul { instructions: String },
}

impl Question {
    /// A `choice` question from `(label, description)` pairs.
    pub fn choice<L, D>(
        instructions: impl Into<String>,
        criteria: impl IntoIterator<Item = (L, D)>,
    ) -> Self
    where
        L: Into<String>,
        D: Into<String>,
    {
        Self::Choice {
            instructions: instructions.into(),
            criteria: criteria
                .into_iter()
                .map(|(label, description)| (label.into(), description.into()))
                .collect(),
        }
    }

    /// A `score` question from an ordered list of level descriptions.
    pub fn score<T>(instructions: impl Into<String>, criteria: impl IntoIterator<Item = T>) -> Self
    where
        T: Into<String>,
    {
        Self::Score {
            instructions: instructions.into(),
            criteria: criteria.into_iter().map(Into::into).collect(),
        }
    }

    /// A `noul` (yes/no) question.
    pub fn noul(instructions: impl Into<String>) -> Self {
        Self::Noul {
            instructions: instructions.into(),
        }
    }
}

/// One decision request: a state plus named typed questions, optionally
/// pinning the server-side model/checkpoint.
#[derive(Debug, Clone, Serialize)]
pub struct DecisionRequest {
    /// The state under evaluation (free-form JSON: text, a record, a diff, …).
    pub state: serde_json::Value,
    /// Named questions, in the wire shape.
    pub questions: serde_json::Map<String, serde_json::Value>,
    /// Optional model/checkpoint passthrough; servers that do not support it
    /// ignore it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

impl DecisionRequest {
    /// An empty request over `state`.
    pub fn new(state: impl Into<serde_json::Value>) -> Self {
        Self {
            state: state.into(),
            questions: serde_json::Map::new(),
            model: None,
        }
    }

    /// Adds one named question.
    pub fn ask(mut self, name: impl Into<String>, question: Question) -> Self {
        self.questions.insert(
            name.into(),
            serde_json::to_value(question).expect("Question always serializes"),
        );
        self
    }

    /// Pins the server-side model/checkpoint when the server supports it.
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }
}

/// Token usage reported by the endpoint, when it reports any.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
}

/// A decision response: one answer object per named question, plus optional
/// usage and routing metadata. Unknown fields are preserved in the raw answer
/// values.
#[derive(Debug, Clone, Deserialize)]
pub struct DecisionResponse {
    #[serde(default)]
    pub answers: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    pub usage: Option<Usage>,
    #[serde(default)]
    pub routing: Option<serde_json::Value>,
}

impl DecisionResponse {
    /// The raw answer object for `question`, if the endpoint returned one.
    pub fn answer(&self, question: &str) -> Option<&serde_json::Value> {
        self.answers.get(question)
    }

    /// The reported answer probability for `question`, when the server
    /// provides one. This is the calibrated quantity to threshold on; the
    /// generic `confidence` field is not comparable across providers.
    pub fn answer_confidence(&self, question: &str) -> Option<f32> {
        let answer = self.answer(question)?;
        CONFIDENCE_KEYS.iter().find_map(|key| {
            answer
                .get(*key)
                .and_then(serde_json::Value::as_f64)
                .map(|value| value as f32)
        })
    }

    /// The answer value for `question`: the first present of
    /// `choice` / `score` / `noul`.
    pub fn primary(&self, question: &str) -> Option<&serde_json::Value> {
        let answer = self.answer(question)?;
        ANSWER_KEYS.iter().find_map(|key| answer.get(*key))
    }
}

/// How a `choice` answer mapped back onto the caller's option set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChoiceResolution {
    /// The endpoint chose an option; `value` is the caller's value for the
    /// chosen `label`.
    Selected { label: String, value: String },
    /// The endpoint chose the explicit "none" option.
    None,
    /// The endpoint returned a label that is not in the option set (a
    /// protocol surprise the caller should surface, not silently drop).
    Unknown(String),
}

/// Maps a single-`choice` answer back onto `labels` (pairs of the label the
/// model saw and the caller's value). `none_label` is the explicit
/// "no option applies" label. A missing answer is [`ChoiceResolution::None`].
pub fn resolve_choice(
    response: &DecisionResponse,
    question: &str,
    none_label: &str,
    labels: &[(String, String)],
) -> ChoiceResolution {
    let Some(label) = response.primary(question).and_then(|v| v.as_str()) else {
        return ChoiceResolution::None;
    };
    if label == none_label {
        return ChoiceResolution::None;
    }
    match labels.iter().find(|(known, _)| known == label) {
        Some((label, value)) => ChoiceResolution::Selected {
            label: label.clone(),
            value: value.clone(),
        },
        None => ChoiceResolution::Unknown(label.to_string()),
    }
}

/// A decision backend. Implementations are shared and may hold locks, so they
/// are `Send + Sync`; [`DecisionProvider::decide`] is blocking.
pub trait DecisionProvider: Send + Sync {
    /// Whether this provider is configured and usable. Cheap and I/O-free
    /// (it does not probe the network).
    fn is_enabled(&self) -> bool;

    /// Runs one decision request. Blocking: call off async workers.
    fn decide(&self, request: &DecisionRequest) -> Result<DecisionResponse>;

    /// Best-effort reachability check. Providers without a health endpoint
    /// default to `Ok(())`.
    fn probe(&self) -> Result<()> {
        Ok(())
    }
}

/// The no-op provider: decisions are disabled and every call fails with an
/// actionable message. The default when `decision.enabled = false`.
#[derive(Debug, Default, Clone, Copy)]
pub struct Disabled;

impl DecisionProvider for Disabled {
    fn is_enabled(&self) -> bool {
        false
    }

    fn decide(&self, _request: &DecisionRequest) -> Result<DecisionResponse> {
        Err(Error::Decision {
            reason: "no decision endpoint is enabled; set `decision.enabled = true` and run a \
                     Jev-compatible server (e.g. `laya-serve`), or leave the feature off"
                .to_string(),
        })
    }
}

/// A blocking client for a Jev-compatible HTTP endpoint.
pub struct JevClient {
    http: reqwest::blocking::Client,
    endpoint: String,
    api_key: Option<String>,
    /// Configured checkpoint/model passthrough, applied to requests that do
    /// not set one.
    model: Option<String>,
    /// Estimated input-token budget every request is fitted to before
    /// sending ([`budget::fit_request`]).
    max_input_tokens: usize,
    health: Mutex<Health>,
}

#[derive(Default)]
struct Health {
    /// While set and in the future, the endpoint is treated as unreachable
    /// without a network attempt.
    down_until: Option<Instant>,
}

impl JevClient {
    /// Builds a client from configuration. Fails when no endpoint can be
    /// resolved (an enabled `jev` with no explicit `endpoint`) or when the
    /// HTTP client cannot be constructed.
    pub fn new(config: &DecisionConfig) -> Result<Self> {
        let endpoint = config
            .endpoint()
            .ok_or_else(|| Error::Decision {
                reason: "decision.endpoint is required for the configured provider".to_string(),
            })?
            .trim_end_matches('/')
            .to_string();
        let http = reqwest::blocking::Client::builder()
            .timeout(config.timeout())
            .build()
            .map_err(|source| Error::Decision {
                reason: format!("failed to build the HTTP client: {source}"),
            })?;
        let api_key = std::env::var(config.api_key_env())
            .ok()
            .filter(|key| !key.trim().is_empty());
        Ok(Self {
            http,
            endpoint,
            api_key,
            model: config.model.clone(),
            max_input_tokens: config.max_input_tokens,
            health: Mutex::new(Health::default()),
        })
    }

    /// The request as it will be sent: the configured model applied when
    /// the caller left it open, then fitted to the input-token budget so
    /// the endpoint's context window is never overflowed (longest fields
    /// trimmed first, each cut marked in place).
    fn prepared(&self, request: &DecisionRequest) -> DecisionRequest {
        let mut owned = request.clone();
        if owned.model.is_none() {
            owned.model = self.model.clone();
        }
        budget::fit_request(&mut owned, self.max_input_tokens);
        owned
    }

    /// The resolved base URL (no trailing slash).
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    fn is_down(&self) -> bool {
        let health = self.health.lock().unwrap_or_else(|e| e.into_inner());
        health
            .down_until
            .is_some_and(|until| Instant::now() < until)
    }

    fn mark_down(&self) {
        let mut health = self.health.lock().unwrap_or_else(|e| e.into_inner());
        health.down_until = Some(Instant::now() + DOWN_COOLDOWN);
    }

    fn clear_down(&self) {
        let mut health = self.health.lock().unwrap_or_else(|e| e.into_inner());
        health.down_until = None;
    }
}

impl DecisionProvider for JevClient {
    fn is_enabled(&self) -> bool {
        true
    }

    fn decide(&self, request: &DecisionRequest) -> Result<DecisionResponse> {
        if self.is_down() {
            return Err(Error::Decision {
                reason: format!(
                    "decision endpoint `{}` is marked unreachable after a recent failure; \
                     start it or disable `decision.enabled`",
                    self.endpoint
                ),
            });
        }
        let url = format!("{}/v1/systemone", self.endpoint);
        let owned = self.prepared(request);
        let mut builder = self.http.post(&url).json(&owned);
        if let Some(key) = &self.api_key {
            builder = builder.bearer_auth(key);
        }
        let response = match builder.send() {
            Ok(response) => response,
            Err(source) => {
                self.mark_down();
                return Err(Error::Decision {
                    reason: format!("decision request to `{url}` failed: {source}"),
                });
            }
        };
        let status = response.status();
        if !status.is_success() {
            let body = response.text().unwrap_or_default();
            // Application errors (bad request, unauthorized, too many options)
            // are the caller's to fix; never cooldown the endpoint for them.
            if status.is_server_error() {
                self.mark_down();
            }
            return Err(Error::Decision {
                reason: format!(
                    "decision endpoint `{}` returned HTTP {status}: {}",
                    self.endpoint,
                    snippet(&body)
                ),
            });
        }
        self.clear_down();
        response
            .json::<DecisionResponse>()
            .map_err(|source| Error::Decision {
                reason: format!(
                    "decision endpoint `{}` returned an unparseable response: {source}",
                    self.endpoint
                ),
            })
    }

    fn probe(&self) -> Result<()> {
        let url = format!("{}/health", self.endpoint);
        match self.http.get(&url).send() {
            Ok(response) if response.status().is_success() => {
                self.clear_down();
                Ok(())
            }
            Ok(response) => {
                self.mark_down();
                Err(Error::Decision {
                    reason: format!(
                        "decision endpoint `{}` health check returned HTTP {}",
                        self.endpoint,
                        response.status()
                    ),
                })
            }
            Err(source) => {
                self.mark_down();
                Err(Error::Decision {
                    reason: format!("decision endpoint `{url}` is unreachable: {source}"),
                })
            }
        }
    }
}

/// Builds the configured provider: [`Disabled`] when decisions are off, a
/// [`JevClient`] otherwise. The returned box is what consumers hold.
pub fn provider_from_config(config: &DecisionConfig) -> Result<Box<dyn DecisionProvider>> {
    if !config.enabled {
        return Ok(Box::new(Disabled));
    }
    match config.provider {
        DecisionProviderConfig::LayaServe | DecisionProviderConfig::Jev => {
            Ok(Box::new(JevClient::new(config)?))
        }
    }
}

/// A short, single-line excerpt of a response body for error messages.
fn snippet(body: &str) -> String {
    let trimmed = body.trim();
    let mut out: String = trimmed.chars().take(200).collect();
    if trimmed.chars().count() > 200 {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn questions_serialize_to_the_wire_shape() {
        let noul = serde_json::to_value(Question::noul("Is it urgent?")).unwrap();
        assert_eq!(noul["type"], "noul");
        assert_eq!(noul["instructions"], "Is it urgent?");

        let choice = serde_json::to_value(Question::choice(
            "Which team?",
            [
                ("billing", "invoices and refunds"),
                ("tech", "bugs and outages"),
            ],
        ))
        .unwrap();
        assert_eq!(choice["type"], "choice");
        assert_eq!(choice["criteria"]["billing"], "invoices and refunds");

        let score = serde_json::to_value(Question::score(
            "How urgent?",
            ["not urgent", "soon", "blocking"],
        ))
        .unwrap();
        assert_eq!(score["type"], "score");
        assert_eq!(score["criteria"][2], "blocking");
    }

    #[test]
    fn request_builder_collects_state_questions_and_model() {
        let request = DecisionRequest::new(serde_json::json!({"body": "disk full"}))
            .ask("urgent", Question::noul("Is it urgent?"))
            .with_model("english");
        let value = serde_json::to_value(&request).unwrap();
        assert_eq!(value["state"]["body"], "disk full");
        assert_eq!(value["questions"]["urgent"]["type"], "noul");
        assert_eq!(value["model"], "english");
    }

    #[test]
    fn response_exposes_answer_confidence_and_primary_faithfully() {
        let response: DecisionResponse = serde_json::from_value(serde_json::json!({
            "answers": {
                "urgent": {"noul": 0.91, "answer_confidence": 0.91, "confidence": 0.4},
                "dept": {"choice": "billing", "confidence": 0.5},
                "level": {"score": 1.84}
            },
            "usage": {"input_tokens": 12, "output_tokens": 3},
            "routing": {"model": "english"}
        }))
        .unwrap();
        // answer_confidence preferred over confidence when both are present.
        assert_eq!(response.answer_confidence("urgent"), Some(0.91));
        // Falls back to confidence when answer_confidence is absent.
        assert_eq!(response.answer_confidence("dept"), Some(0.5));
        assert_eq!(response.answer_confidence("level"), None);
        assert_eq!(response.primary("urgent").unwrap().as_f64(), Some(0.91));
        assert_eq!(response.primary("dept").unwrap().as_str(), Some("billing"));
        assert_eq!(response.primary("level").unwrap().as_f64(), Some(1.84));
        assert!(response.primary("missing").is_none());
        assert_eq!(response.usage.unwrap().input_tokens, 12);
        assert_eq!(response.routing.unwrap()["model"], "english");
    }

    #[test]
    fn disabled_provider_refuses_with_an_actionable_message() {
        let provider = Disabled;
        assert!(!provider.is_enabled());
        let error = provider
            .decide(&DecisionRequest::new(serde_json::json!("x")))
            .unwrap_err();
        assert!(matches!(error, Error::Decision { .. }));
        assert!(error.to_string().contains("decision.enabled"));
    }

    #[test]
    fn provider_from_config_is_disabled_by_default() {
        let provider = provider_from_config(&DecisionConfig::default()).unwrap();
        assert!(!provider.is_enabled());
    }

    #[test]
    fn jev_provider_requires_an_explicit_endpoint() {
        let config = DecisionConfig {
            enabled: true,
            provider: DecisionProviderConfig::Jev,
            endpoint: None,
            ..DecisionConfig::default()
        };
        assert!(JevClient::new(&config).is_err());
    }

    #[test]
    fn client_resolves_provider_defaults_and_trims_trailing_slash() {
        let config = DecisionConfig {
            endpoint: Some("http://127.0.0.1:8000/".to_string()),
            ..DecisionConfig::default()
        };
        let client = JevClient::new(&config).unwrap();
        assert_eq!(client.endpoint(), "http://127.0.0.1:8000");
    }

    #[test]
    fn prepared_applies_the_default_model_and_fits_the_budget() {
        let config = DecisionConfig {
            endpoint: Some("http://127.0.0.1:8000".to_string()),
            model: Some("english".to_string()),
            max_input_tokens: 64,
            ..DecisionConfig::default()
        };
        let client = JevClient::new(&config).unwrap();

        // Within budget: only the model default is applied.
        let small = DecisionRequest::new(serde_json::json!({"question": "what is a bundle?"}));
        let prepared = client.prepared(&small);
        assert_eq!(prepared.model.as_deref(), Some("english"));
        assert_eq!(prepared.state, small.state);

        // An explicit request-side model wins over the configured one.
        let pinned = small.clone().with_model("typed-decisions");
        assert_eq!(
            client.prepared(&pinned).model.as_deref(),
            Some("typed-decisions")
        );

        // Over budget: the bulk field is trimmed and marked in place.
        let large = DecisionRequest::new(serde_json::json!({
            "code_under_review": "let x = 1;\n".repeat(500),
        }));
        let prepared = client.prepared(&large);
        let code = prepared.state["code_under_review"].as_str().unwrap();
        assert!(code.contains("chars cut…"));
        assert!(budget::request_tokens(&prepared) <= 64);
    }

    #[test]
    fn resolve_choice_maps_labels_and_handles_none_and_unknown() {
        let labels = vec![
            ("A".to_string(), "uri-a".to_string()),
            ("B".to_string(), "uri-b".to_string()),
        ];
        let chosen: DecisionResponse =
            serde_json::from_value(serde_json::json!({"answers": {"answer": {"choice": "B"}}}))
                .unwrap();
        assert_eq!(
            resolve_choice(&chosen, "answer", "NONE", &labels),
            ChoiceResolution::Selected {
                label: "B".into(),
                value: "uri-b".into()
            }
        );

        let none: DecisionResponse =
            serde_json::from_value(serde_json::json!({"answers": {"answer": {"choice": "NONE"}}}))
                .unwrap();
        assert_eq!(
            resolve_choice(&none, "answer", "NONE", &labels),
            ChoiceResolution::None
        );

        let unknown: DecisionResponse =
            serde_json::from_value(serde_json::json!({"answers": {"answer": {"choice": "Z"}}}))
                .unwrap();
        assert_eq!(
            resolve_choice(&unknown, "answer", "NONE", &labels),
            ChoiceResolution::Unknown("Z".into())
        );

        let missing: DecisionResponse =
            serde_json::from_value(serde_json::json!({"answers": {}})).unwrap();
        assert_eq!(
            resolve_choice(&missing, "answer", "NONE", &labels),
            ChoiceResolution::None
        );
    }
}
