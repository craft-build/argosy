//! Integration tests for the MCP server: the rmcp transport layer
//! over an in-process duplex (no stdio child process, no network, no ONNX).
#![cfg(feature = "mcp")]
//!
//! Handler-level coverage of every tool and resource lives in
//! `src/mcp.rs`'s unit tests with the crate's internal doubles. This file
//! drives the *wire*: a real rmcp client over a real rmcp `ServerHandler`,
//! with local public-trait doubles here (`pub(crate)` is invisible).

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use argosy::Namespace;
use argosy::context::{ProjectContext, QualifiedConceptId};
use argosy::index::{EmbeddingProvider, EmbeddingUnit, Filter, Index, SearchHit, VectorStore};
use argosy::mcp::{ArgosyMcpServer, McpState};
use argosy::{Concept, LocalArgosy, Result};
use tempfile::TempDir;

// --- Public-trait backend doubles (no ONNX, no SQLite) ---------------------

const DIM: usize = 64;

/// Deterministic token-bucket embedder: normalized tokens hash into
/// dimensions, so overlapping texts score positively under cosine, like the
/// crate's `MockEmbedder` (duplicated here because test doubles are
/// `pub(crate)`).
struct FakeEmbedder;

impl FakeEmbedder {
    fn embed_one(text: &str) -> Vec<f32> {
        let mut v = vec![0f32; DIM];
        let normalized: String = text
            .to_lowercase()
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { ' ' })
            .collect();
        for token in normalized.split_whitespace() {
            let mut hash: u32 = 0x811c_9dc5;
            for byte in token.bytes() {
                hash = (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193);
            }
            v[(hash as usize) % DIM] += 1.0;
        }
        v
    }
}

impl EmbeddingProvider for FakeEmbedder {
    fn model_id(&self) -> &str {
        "fake-embedder@1"
    }

    fn dimensions(&self) -> usize {
        DIM
    }

    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| Self::embed_one(t)).collect())
    }
}

/// In-memory cosine store over the public [`VectorStore`] trait.
#[derive(Default)]
struct MemVec {
    model_id: Option<String>,
    units: HashMap<(QualifiedConceptId, u32), EmbeddingUnit>,
}

fn matches_filter(unit: &EmbeddingUnit, filter: &Filter) -> bool {
    let qid = &unit.concept;
    if let Some(ns) = &filter.namespaces
        && !ns.contains(&qid.namespace)
    {
        return false;
    }
    if let Some(argosies) = &filter.argosies
        && !argosies.contains(&qid.argosy)
    {
        return false;
    }
    if let Some(types) = &filter.concept_types
        && unit
            .meta
            .concept_type
            .as_ref()
            .is_none_or(|t| !types.contains(t))
    {
        return false;
    }
    if let Some(tags) = &filter.tags {
        if tags.is_empty() {
            return false;
        }
        if !tags.iter().any(|t| unit.meta.tags.contains(t)) {
            return false;
        }
    }
    if let Some(language) = &filter.language
        && unit.meta.language.as_deref() != Some(language.as_str())
    {
        return false;
    }
    if let Some(category) = &filter.category
        && unit.meta.category.as_deref() != Some(category.as_str())
    {
        return false;
    }
    true
}

impl VectorStore for MemVec {
    fn model_id(&self) -> Option<&str> {
        self.model_id.as_deref()
    }

    fn set_model_id(&mut self, id: &str) -> Result<()> {
        self.model_id = Some(id.to_string());
        Ok(())
    }

    fn upsert(&mut self, units: &[EmbeddingUnit]) -> Result<()> {
        for unit in units {
            self.units
                .insert((unit.concept.clone(), unit.chunk_ordinal), unit.clone());
        }
        Ok(())
    }

    fn remove_concept(&mut self, concept: &QualifiedConceptId) -> Result<()> {
        self.units.retain(|(qid, _), _| qid != concept);
        Ok(())
    }

    fn unit_hashes(&self) -> Result<HashMap<QualifiedConceptId, String>> {
        let mut out = HashMap::new();
        for unit in self.units.values() {
            out.insert(unit.concept.clone(), unit.text_hash.clone());
        }
        Ok(out)
    }

    fn clear(&mut self) -> Result<()> {
        self.units.clear();
        self.model_id = None;
        Ok(())
    }

    fn search(&self, vector: &[f32], k: usize, filter: &Filter) -> Result<Vec<SearchHit>> {
        let dot = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
        let norm = |a: &[f32]| dot(a, a).sqrt();
        let qnorm = norm(vector);
        let mut hits: Vec<SearchHit> = self
            .units
            .values()
            .filter(|u| matches_filter(u, filter))
            .map(|u| {
                let denom = qnorm * norm(&u.vector);
                let score = if denom > 0.0 {
                    dot(vector, &u.vector) / denom
                } else {
                    0.0
                };
                SearchHit {
                    concept: u.concept.clone(),
                    score,
                    meta: u.meta.clone(),
                }
            })
            .collect();
        hits.sort_by(|a, b| b.score.total_cmp(&a.score));
        hits.truncate(k);
        Ok(hits)
    }
}

// --- Fixtures --------------------------------------------------------------

/// Copies a shared fixture into a fresh tempdir — tests must never mutate
/// `tests/fixtures/` directly.
fn fixture_copy(name: &str) -> TempDir {
    fn copy_dir_all(src: &Path, dst: &Path) {
        for entry in fs::read_dir(src).unwrap() {
            let entry = entry.unwrap();
            let to = dst.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                fs::create_dir_all(&to).unwrap();
                copy_dir_all(&entry.path(), &to);
            } else {
                fs::copy(entry.path(), to).unwrap();
            }
        }
    }
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let dst = tempfile::tempdir().unwrap();
    copy_dir_all(&src, dst.path());
    dst
}

/// An imported argosy with one `machine-confirmed` skill.
fn import_fixture() -> TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let local = LocalArgosy::init(tmp.path(), Some("acme-shared"), None).unwrap();
    let skill: Concept = ("---\n\
         type: Skill\n\
         description: Audit the shared provisioner.\n\
         verified: machine-confirmed\n\
         ---\n\
         # Audit\n\n\
         Steps.\n")
        .parse()
        .unwrap();
    local
        .write_concept(
            Namespace::Skill,
            &"skill/shared-audit".parse().unwrap(),
            &skill,
        )
        .unwrap();
    tmp
}

struct Rig {
    _local: TempDir,
    _imported: TempDir,
    state: McpState<FakeEmbedder, MemVec>,
}

fn rig() -> Rig {
    let local = fixture_copy("valid-acme-billing");
    let imported = import_fixture();
    let context = ProjectContext::open(local.path(), [imported.path().to_path_buf()]).unwrap();
    let mut index = Index::new(FakeEmbedder, MemVec::default());
    index.reconcile(&context).unwrap();
    Rig {
        _local: local,
        _imported: imported,
        state: McpState::new(context, index),
    }
}

// --- Structural invariants -------------------------------------------------

/// Imported argosys are read-only *structurally* — no
/// mutating tool may expose an `argosy` selector. If a future tool violates
/// this, the invariant breaks here, not in a runtime exploit.
#[test]
fn write_tools_have_no_argosy_selector_in_their_schemas() {
    let tools = argosy::mcp::tool_definitions();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    for expected in [
        "search",
        "list_skills",
        "get_skill",
        "search_rules",
        "read_memory",
        "write_memory",
        "delete_memory",
        "write_rule",
        "delete_rule",
        "promote",
    ] {
        assert!(names.contains(&expected), "missing tool `{expected}`");
    }
    assert_eq!(tools.len(), 10, "exactly the doc-10 tool set");

    for tool in &tools {
        let props = tool.input_schema["properties"]
            .as_object()
            .cloned()
            .unwrap_or_default();
        if [
            "write_memory",
            "delete_memory",
            "write_rule",
            "delete_rule",
            "promote",
        ]
        .contains(&tool.name.as_ref())
        {
            assert!(
                !props.contains_key("argosy"),
                "mutating tool `{}` must not take an argosy selector",
                tool.name
            );
        } else if tool.name.as_ref() == "search" {
            assert!(props.contains_key("argosy"), "search scopes by argosy");
        }
        // Every tool carries an LLM-facing description.
        assert!(
            tool.description.as_deref().is_some_and(|d| d.len() > 40),
            "tool `{}` needs a real description",
            tool.name
        );
    }
}

// --- End-to-end over an in-process duplex ---------------------------------

fn call(name: &str, arguments: serde_json::Value) -> rmcp::model::CallToolRequestParams {
    let mut params = rmcp::model::CallToolRequestParams::new(name.to_string());
    params.arguments = Some(arguments.as_object().expect("object args").clone());
    params
}

/// Unwraps a completed tool result; MRTR task/input outcomes are never
/// emitted by this server.
fn complete(response: rmcp::model::CallToolResponse) -> rmcp::model::CallToolResult {
    match response {
        rmcp::model::CallToolResponse::Complete(result) => result,
        other => panic!("expected a complete result, got {other:?}"),
    }
}

fn structured(result: &rmcp::model::CallToolResult) -> serde_json::Value {
    result
        .structured_content
        .clone()
        .expect("structured output present")
}

#[tokio::test(flavor = "multi_thread")]
async fn end_to_end_over_in_process_duplex() {
    let rig = rig();
    let (server_io, client_io) = tokio::io::duplex(8192);

    let server = ArgosyMcpServer::new(rig.state);
    let server_task = tokio::spawn(async move {
        use rmcp::ServiceExt;
        let running = server.serve(server_io).await.expect("server initializes");
        // `waiting()` (not `cancel()`, which shuts down immediately): serve
        // until the client drops its end of the duplex.
        running.waiting().await
    });

    // `()` is rmcp's minimal client handler; serve() completes the handshake.
    use rmcp::ServiceExt;
    let client = ().serve(client_io).await.expect("initialize handshake");

    // list_skills: trust tier surfaced for local (unverified) and imported
    // (machine-confirmed) skills.
    let skills = complete(
        client
            .call_tool_once(call("list_skills", serde_json::json!({})))
            .await
            .unwrap(),
    );
    let skills = structured(&skills);
    let shared = skills["skills"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "shared-audit")
        .expect("imported skill listed");
    assert_eq!(shared["argosy"], "acme-shared");
    assert_eq!(shared["verified"], "machine-confirmed");
    assert!(
        skills["skills"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["name"] == "reconcile-ledger" && s["verified"] == "unverified")
    );

    // search: semantic query returns qualified URIs.
    let search = complete(
        client
            .call_tool_once(call(
                "search",
                serde_json::json!({"query": "rate limit retries"}),
            ))
            .await
            .unwrap(),
    );
    let search = structured(&search);
    assert!(
        search["hits"].as_array().unwrap().iter().any(|h| h["uri"]
            .as_str()
            .unwrap()
            .starts_with("argosy://acme-billing/")),
        "qualified hits, got {search}"
    );

    // write_memory → read_memory verifies the write round trips, and the
    // same-session search sees it (the index reconciles on every write).
    let content = "---\ntype: Session Note\ndescription: e2e\n---\n# E2E\n\nBody.\n";
    let written = complete(
        client
            .call_tool_once(call(
                "write_memory",
                serde_json::json!({
                    "path": "memory/e2e-note",
                    "content": content,
                }),
            ))
            .await
            .unwrap(),
    );
    let written = structured(&written);
    assert_eq!(written["uri"], "argosy://acme-billing/memory/e2e-note");
    assert_eq!(written["action"], "created");
    assert_eq!(written["indexed"], true, "the write reconciled the index");

    let fresh = complete(
        client
            .call_tool_once(call(
                "search",
                serde_json::json!({"query": "e2e note body", "namespaces": ["memory"]}),
            ))
            .await
            .unwrap(),
    );
    let fresh = structured(&fresh);
    assert!(
        fresh["hits"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["uri"].as_str().unwrap().ends_with("memory/e2e-note")),
        "the fresh write is searchable in the same session, got {fresh}"
    );

    let read_back = complete(
        client
            .call_tool_once(call(
                "read_memory",
                serde_json::json!({"path": "memory/e2e-note"}),
            ))
            .await
            .unwrap(),
    );
    assert!(
        structured(&read_back)["content"]
            .as_str()
            .unwrap()
            .contains("# E2E")
    );

    // promote to document, then read the promoted concept as a resource.
    let promoted = complete(
        client
            .call_tool_once(call(
                "promote",
                serde_json::json!({
                    "source_path": "memory/e2e-note",
                    "target": "document",
                    "new_path": "document/e2e-promoted",
                }),
            ))
            .await
            .unwrap(),
    );
    let promoted = structured(&promoted);
    assert_eq!(promoted["target"], "document");
    assert_eq!(
        promoted["new_uri"],
        "argosy://acme-billing/document/e2e-promoted"
    );

    let resource = client
        .read_resource(rmcp::model::ReadResourceRequestParams::new(
            "argosy://acme-billing/document/e2e-promoted",
        ))
        .await
        .unwrap();
    let text = match &resource.contents[0] {
        rmcp::model::ResourceContents::TextResourceContents { text, .. } => text,
        other => panic!("expected text contents, got {other:?}"),
    };
    assert!(text.contains("type: Session Note"), "frontmatter survives");

    // The argosys listing resource.
    let argosys = client
        .read_resource(rmcp::model::ReadResourceRequestParams::new(
            argosy::mcp::ARGOSYS_URI,
        ))
        .await
        .unwrap();
    let text = match &argosys.contents[0] {
        rmcp::model::ResourceContents::TextResourceContents { text, .. } => text,
        other => panic!("expected text contents, got {other:?}"),
    };
    assert!(
        text.contains("acme-shared"),
        "imported argosy listed: {text}"
    );

    // Unknown resource: protocol-level resource-not-found, not empty content.
    let missing = client
        .read_resource(rmcp::model::ReadResourceRequestParams::new(
            "argosy://acme-billing/memory/nope",
        ))
        .await;
    assert!(missing.is_err(), "unknown concept must be an error");

    // Tool-level failure stays a tool error: unknown tool arguments.
    let bad = complete(
        client
            .call_tool_once(call(
                "write_memory",
                serde_json::json!({"path": "../escape"}),
            ))
            .await
            .unwrap(),
    );
    assert_eq!(bad.is_error, Some(true), "bad write is a tool error");

    drop(client);
    server_task.abort();
}
