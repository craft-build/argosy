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
use std::path::{Path, PathBuf};
use std::sync::Arc;

use argosy::Namespace;
use argosy::context::{ProjectContext, QualifiedConceptId};
use argosy::index::{EmbeddingProvider, EmbeddingUnit, Filter, Index, SearchHit, VectorStore};
use argosy::mcp::{ArgosyMcpServer, McpState, ProjectSession, SessionFactory};
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
    /// The cwd tool calls name: the local fixture's path (the factory maps
    /// any root to the fixture session, so this is a stand-in root).
    cwd: PathBuf,
    state: McpState<FakeEmbedder, MemVec>,
}

fn rig() -> Rig {
    let local = fixture_copy("valid-acme-billing");
    let imported = import_fixture();
    let factory: SessionFactory<FakeEmbedder, MemVec> = {
        let local_root = local.path().to_path_buf();
        let imported_root = imported.path().to_path_buf();
        Arc::new(move |_root| {
            let context = ProjectContext::open(&local_root, [imported_root.clone()])?;
            let mut index = Index::new(FakeEmbedder, MemVec::default());
            index.reconcile(&context)?;
            Ok(ProjectSession::new(context, index))
        })
    };
    Rig {
        cwd: local.path().to_path_buf(),
        _local: local,
        _imported: imported,
        state: McpState::new(factory),
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
        "write_document",
        "delete_document",
        "promote",
    ] {
        assert!(names.contains(&expected), "missing tool `{expected}`");
    }
    #[cfg(feature = "code-tools")]
    for expected in [
        "outline",
        "zoom",
        "astgrep",
        "conflicts",
        "inspect",
        "callgraph",
        "repomap",
    ] {
        assert!(names.contains(&expected), "missing tool `{expected}`");
    }
    #[cfg(feature = "code-tools")]
    let expected_total = 19;
    #[cfg(not(feature = "code-tools"))]
    let expected_total = 12;
    assert_eq!(
        tools.len(),
        expected_total,
        "exactly the documented tool set"
    );

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
            "write_document",
            "delete_document",
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
    let cwd = rig.cwd.clone();
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
            .call_tool_once(call("list_skills", serde_json::json!({"cwd": &cwd})))
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
                serde_json::json!({"cwd": &cwd, "query": "rate limit retries"}),
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
                    "cwd": &cwd,
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
                serde_json::json!({
                    "cwd": &cwd,
                    "query": "e2e note body",
                    "namespaces": ["memory"]
                }),
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
                serde_json::json!({"cwd": &cwd, "path": "memory/e2e-note"}),
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

    // write_document → same-session search sees it → delete_document makes
    // it disappear (the index reconciles on every mutation).
    let doc = "---\ntype: Decision\ndescription: e2e doc\n---\n# E2E Doc\n\nWe decide.\n";
    let written = complete(
        client
            .call_tool_once(call(
                "write_document",
                serde_json::json!({
                    "cwd": &cwd,
                    "path": "document/e2e-decision",
                    "content": doc,
                }),
            ))
            .await
            .unwrap(),
    );
    let written = structured(&written);
    assert_eq!(
        written["uri"],
        "argosy://acme-billing/document/e2e-decision"
    );
    assert_eq!(written["action"], "created");
    assert_eq!(written["indexed"], true, "the write reconciled the index");

    let fresh_doc = complete(
        client
            .call_tool_once(call(
                "search",
                serde_json::json!({
                    "cwd": &cwd,
                    "query": "e2e decision doc",
                    "namespaces": ["document"]
                }),
            ))
            .await
            .unwrap(),
    );
    let fresh_doc = structured(&fresh_doc);
    assert!(
        fresh_doc["hits"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["uri"]
                .as_str()
                .unwrap()
                .ends_with("document/e2e-decision")),
        "the fresh document is searchable in the same session, got {fresh_doc}"
    );

    let deleted = complete(
        client
            .call_tool_once(call(
                "delete_document",
                serde_json::json!({"cwd": &cwd, "path": "document/e2e-decision"}),
            ))
            .await
            .unwrap(),
    );
    assert_eq!(structured(&deleted)["action"], "deleted");

    // promote to document, then read the promoted concept as a resource.
    let promoted = complete(
        client
            .call_tool_once(call(
                "promote",
                serde_json::json!({
                    "cwd": &cwd,
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
                serde_json::json!({"cwd": &cwd, "path": "../escape"}),
            ))
            .await
            .unwrap(),
    );
    assert_eq!(bad.is_error, Some(true), "bad write is a tool error");

    // Prompts: the dream memory-consolidation workflow.
    let prompts = client.list_prompts(None).await.unwrap();
    assert_eq!(prompts.prompts.len(), 1, "exactly one advertised prompt");
    assert_eq!(prompts.prompts[0].name, "dream");
    assert!(
        prompts.prompts[0]
            .description
            .as_deref()
            .is_some_and(|d| !d.is_empty()),
        "dream carries a description"
    );

    let dream = client
        .get_prompt(rmcp::model::GetPromptRequestParams::new("dream"))
        .await
        .unwrap();
    assert_eq!(dream.messages.len(), 1);
    assert_eq!(dream.messages[0].role, rmcp::model::Role::User);
    match &dream.messages[0].content {
        rmcp::model::ContentBlock::Text(text) => {
            // Self-contained workflow: the tools it drives are all named.
            for tool in ["search", "read_memory", "write_memory", "delete_memory"] {
                assert!(text.text.contains(tool), "dream names `{tool}`");
            }
            assert!(text.text.contains("no-op is a valid outcome"));
        }
        other => panic!("expected text content, got {other:?}"),
    }

    // Unknown prompt name: protocol-level method-not-found, not empty content.
    let missing_prompt = client
        .get_prompt(rmcp::model::GetPromptRequestParams::new("nope"))
        .await;
    assert!(missing_prompt.is_err(), "unknown prompt must be an error");

    drop(client);
    server_task.abort();
}

// --- Multi-project behavior over the wire ----------------------------------

/// The server needs no project at startup and serves several projects from
/// one process: each tool call's `cwd` selects (and lazily opens) its own
/// session, an argosy-less cwd is a tool-level error, and the sessions are
/// isolated from each other.
#[tokio::test(flavor = "multi_thread")]
async fn tools_select_their_project_by_cwd() {
    // Real project layouts, opened by the real discovery path (globals
    // redirected to an empty tempdir so the user store cannot leak in).
    let globals = tempfile::tempdir().unwrap();
    let globals_root = globals.path().to_path_buf();
    let factory: SessionFactory<FakeEmbedder, MemVec> = Arc::new(move |root| {
        let context = ProjectContext::open_project_with_globals(root, &globals_root)?;
        let mut index = Index::new(FakeEmbedder, MemVec::default());
        index.reconcile(&context)?;
        Ok(ProjectSession::new(context, index))
    });
    let alpha = tempfile::tempdir().unwrap();
    let beta = tempfile::tempdir().unwrap();
    LocalArgosy::init(alpha.path().join(".argosy/default"), Some("alpha"), None).unwrap();
    LocalArgosy::init(beta.path().join(".argosy/default"), Some("beta"), None).unwrap();

    let (server_io, client_io) = tokio::io::duplex(8192);
    let server_task = tokio::spawn(async move {
        use rmcp::ServiceExt;
        let running = ArgosyMcpServer::new(McpState::new(factory))
            .serve(server_io)
            .await
            .expect("server initializes without any project");
        let _ = running.waiting().await;
    });
    use rmcp::ServiceExt;
    let client = ().serve(client_io).await.expect("initialize handshake");

    // Writes land in the project named by cwd, and each project sees only
    // its own write.
    for (root, name) in [(alpha.path(), "alpha"), (beta.path(), "beta")] {
        let written = complete(
            client
                .call_tool_once(call(
                    "write_memory",
                    serde_json::json!({
                        "cwd": root,
                        "path": "memory/who-am-i",
                        "content": format!(
                            "---\ntype: Session Note\ndescription: {name}\n---\n# {name}\n"
                        ),
                    }),
                ))
                .await
                .unwrap(),
        );
        assert_eq!(
            structured(&written)["uri"],
            format!("argosy://{name}/memory/who-am-i")
        );
    }

    for (root, name, other) in [
        (alpha.path(), "alpha", "beta"),
        (beta.path(), "beta", "alpha"),
    ] {
        let skills = complete(
            client
                .call_tool_once(call(
                    "search",
                    serde_json::json!({
                        "cwd": root,
                        "query": "who am i",
                        "namespaces": ["memory"],
                        "k": 10,
                    }),
                ))
                .await
                .unwrap(),
        );
        let skills = structured(&skills);
        let uris: Vec<&str> = skills["hits"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|h| h["uri"].as_str())
            .collect();
        assert!(
            uris.iter()
                .any(|u| u.starts_with(&format!("argosy://{name}/"))),
            "{name} sees its own write, got {uris:?}"
        );
        assert!(
            !uris
                .iter()
                .any(|u| u.starts_with(&format!("argosy://{other}/"))),
            "{name} must not see {other}'s write, got {uris:?}"
        );
    }

    // A cwd with no argosy: tool-level error (isError), not a protocol
    // error, pointing at `argosy init`.
    let nowhere = tempfile::tempdir().unwrap();
    let result = complete(
        client
            .call_tool_once(call(
                "list_skills",
                serde_json::json!({"cwd": nowhere.path()}),
            ))
            .await
            .unwrap(),
    );
    assert_eq!(
        result.is_error,
        Some(true),
        "argosy-less cwd is a tool error"
    );
    match &result.content[0] {
        rmcp::model::ContentBlock::Text(text) => {
            assert!(text.text.contains("argosy init"), "got {}", text.text);
        }
        other => panic!("expected text error, got {other:?}"),
    }

    drop(client);
    server_task.abort();
}

// --- Code-intelligence tools over the wire ---------------------------------//
// The seven tools ported from Craft, driven like any MCP client would:
// absolute paths into a tempdir workspace, structured outcomes, tool-level
// errors with `isError`.

#[cfg(feature = "code-tools")]
mod code_tools {
    use super::*;

    /// A small multi-language workspace: Rust + Python + a TODO.
    fn workspace() -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(
            dir.path().join("src/main.rs"),
            "fn main() {\n    helper();\n}\n\nfn helper() {\n    println!(\"hi\");\n}\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("src/app.py"),
            "def run():\n    # TODO: handle errors\n    return 1\n",
        )
        .unwrap();
        fs::write(dir.path().join("notes.txt"), "not source\n").unwrap();
        dir
    }

    type TestClient = rmcp::service::RunningService<rmcp::RoleClient, ()>;

    async fn client() -> (TestClient, tokio::task::JoinHandle<()>) {
        let rig = rig();
        let (server_io, client_io) = tokio::io::duplex(8192);
        let server = ArgosyMcpServer::new(rig.state);
        let server_task = tokio::spawn(async move {
            use rmcp::ServiceExt;
            let running = server.serve(server_io).await.expect("server initializes");
            let _ = running.waiting().await;
        });
        use rmcp::ServiceExt;
        let client = ().serve(client_io).await.expect("initialize handshake");
        (client, server_task)
    }

    async fn call_ok(
        client: &TestClient,
        name: &str,
        args: serde_json::Value,
    ) -> serde_json::Value {
        let result = complete(client.call_tool_once(call(name, args)).await.unwrap());
        assert_ne!(result.is_error, Some(true), "`{name}` must succeed");
        structured(&result)
    }

    async fn call_err(client: &TestClient, name: &str, args: serde_json::Value) -> String {
        let result = complete(client.call_tool_once(call(name, args)).await.unwrap());
        assert_eq!(result.is_error, Some(true), "`{name}` must fail");
        match &result.content[0] {
            rmcp::model::ContentBlock::Text(text) => text.text.clone(),
            other => panic!("expected text error, got {other:?}"),
        }
    }

    fn abs(dir: &TempDir, rel: &str) -> String {
        dir.path().join(rel).to_string_lossy().into_owned()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn outline_file_and_directory() {
        let ws = workspace();
        let (client, server) = client().await;

        // File mode: symbol tree with kinds and line numbers.
        let out = call_ok(
            &client,
            "outline",
            serde_json::json!({"path": abs(&ws, "src/main.rs")}),
        )
        .await;
        assert_eq!(out["truncated"], false);
        assert!(
            out["text"].as_str().unwrap().contains("fn main"),
            "got {out}"
        );
        assert!(out["text"].as_str().unwrap().contains("helper"));

        // Directory mode: per-file trees with a skipped section.
        let out = call_ok(
            &client,
            "outline",
            serde_json::json!({"path": abs(&ws, "src")}),
        )
        .await;
        let text = out["text"].as_str().unwrap();
        assert!(text.contains("app.py"), "got {text}");
        assert!(text.contains("run"), "got {text}");
        assert!(text.contains("total: 2 files"), "got {text}");

        // Files mode: flat table with languages.
        let out = call_ok(
            &client,
            "outline",
            serde_json::json!({"path": abs(&ws, "src"), "files": true}),
        )
        .await;
        let text = out["text"].as_str().unwrap();
        assert!(text.contains("rust"), "got {text}");
        assert!(text.contains("python"), "got {text}");

        // Unsupported language is a report, not an error.
        let out = call_ok(
            &client,
            "outline",
            serde_json::json!({"path": abs(&ws, "notes.txt")}),
        )
        .await;
        assert!(out["text"].as_str().unwrap().contains("unsupported"));

        let err = call_err(
            &client,
            "outline",
            serde_json::json!({"path": abs(&ws, "nope.rs")}),
        )
        .await;
        assert!(err.contains("does not exist"), "{err}");

        drop(client);
        server.abort();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn zoom_symbol_range_and_ambiguity() {
        let ws = workspace();
        let (client, server) = client().await;

        let out = call_ok(
            &client,
            "zoom",
            serde_json::json!({
                "path": abs(&ws, "src/main.rs"),
                "symbol": "helper",
                "context_lines": 0,
            }),
        )
        .await;
        let text = out["text"].as_str().unwrap();
        assert!(text.contains("fn helper"), "got {text}");
        assert!(text.contains("5 |"), "numbered gutter, got {text}");

        let out = call_ok(
            &client,
            "zoom",
            serde_json::json!({
                "path": abs(&ws, "src/main.rs"),
                "start_line": 1,
                "end_line": 2,
            }),
        )
        .await;
        assert!(out["text"].as_str().unwrap().contains("fn main"));

        let err = call_err(
            &client,
            "zoom",
            serde_json::json!({"path": abs(&ws, "src/main.rs")}),
        )
        .await;
        assert!(err.contains("provide either"), "{err}");

        let err = call_err(
            &client,
            "zoom",
            serde_json::json!({
                "path": abs(&ws, "src/main.rs"),
                "symbol": "definitely_not_there",
            }),
        )
        .await;
        assert!(err.contains("not found"), "{err}");

        drop(client);
        server.abort();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn astgrep_search_diff_apply_and_guards() {
        let ws = workspace();
        let (client, server) = client().await;

        // Search mode.
        let out = call_ok(
            &client,
            "astgrep",
            serde_json::json!({
                "pattern": "println!($MSG)",
                "lang": "rust",
                "path": abs(&ws, "src"),
            }),
        )
        .await;
        assert_eq!(out["mode"], "search");
        assert_eq!(out["matches"], 1);
        assert!(
            out["text"].as_str().unwrap().contains("main.rs:6"),
            "got {out}"
        );

        // Dry-run: unified diff, file untouched.
        let before = fs::read_to_string(ws.path().join("src/main.rs")).unwrap();
        let out = call_ok(
            &client,
            "astgrep",
            serde_json::json!({
                "pattern": "println!($MSG)",
                "rewrite": "eprintln!($MSG)",
                "lang": "rust",
                "path": abs(&ws, "src"),
            }),
        )
        .await;
        assert_eq!(out["mode"], "diff");
        assert!(
            out["text"].as_str().unwrap().contains("+    eprintln!"),
            "got {out}"
        );
        assert_eq!(
            fs::read_to_string(ws.path().join("src/main.rs")).unwrap(),
            before
        );

        // Apply: writes and reports.
        let out = call_ok(
            &client,
            "astgrep",
            serde_json::json!({
                "pattern": "println!($MSG)",
                "rewrite": "eprintln!($MSG)",
                "lang": "rust",
                "path": abs(&ws, "src"),
                "apply": true,
            }),
        )
        .await;
        assert_eq!(out["mode"], "apply");
        assert_eq!(out["files_changed"], 1);
        assert_eq!(out["rolled_back"], false);
        assert!(
            fs::read_to_string(ws.path().join("src/main.rs"))
                .unwrap()
                .contains("eprintln!"),
            "the file was rewritten"
        );

        // Syntax-breaking rewrite is rolled back, file untouched.
        fs::write(
            ws.path().join("src/broken.rs"),
            "fn broken() {\n    println!(\"x\");\n}\n",
        )
        .unwrap();
        let before = fs::read_to_string(ws.path().join("src/broken.rs")).unwrap();
        let out = call_ok(
            &client,
            "astgrep",
            serde_json::json!({
                "pattern": "println!($MSG)",
                "rewrite": ")))) not rust",
                "lang": "rust",
                "path": abs(&ws, "src/broken.rs"),
                "apply": true,
            }),
        )
        .await;
        assert_eq!(out["rolled_back"], true);
        assert_eq!(
            fs::read_to_string(ws.path().join("src/broken.rs")).unwrap(),
            before
        );

        // Stale read: dry-run records the read, an external change then
        // blocks the apply.
        let out = call_ok(
            &client,
            "astgrep",
            serde_json::json!({
                "pattern": "println!($MSG)",
                "rewrite": "eprintln!($MSG)",
                "lang": "rust",
                "path": abs(&ws, "src/broken.rs"),
            }),
        )
        .await;
        assert_eq!(out["mode"], "diff");
        fs::write(
            ws.path().join("src/broken.rs"),
            "fn broken() {\n    println!(\"changed\");\n}\n",
        )
        .unwrap();
        let err = call_err(
            &client,
            "astgrep",
            serde_json::json!({
                "pattern": "println!($MSG)",
                "rewrite": "eprintln!($MSG)",
                "lang": "rust",
                "path": abs(&ws, "src/broken.rs"),
                "apply": true,
            }),
        )
        .await;
        assert!(err.contains("changed since last read"), "{err}");

        // Unknown language is a tool error listing the supported set.
        let err = call_err(
            &client,
            "astgrep",
            serde_json::json!({"pattern": "x", "lang": "starlark"}),
        )
        .await;
        assert!(err.contains("unsupported language"), "{err}");

        drop(client);
        server.abort();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn conflicts_list_and_resolve() {
        let ws = workspace();
        fs::write(
            ws.path().join("merged.rs"),
            "top\n<<<<<<< HEAD\nours\n=======\ntheirs\n>>>>>>> feature\nbottom\n",
        )
        .unwrap();
        let (client, server) = client().await;

        let out = call_ok(
            &client,
            "conflicts",
            serde_json::json!({"path": abs(&ws, "merged.rs")}),
        )
        .await;
        let text = out["text"].as_str().unwrap();
        assert!(text.contains("merge conflicts in 1 file(s)"), "got {text}");
        assert!(text.contains("HEAD vs feature"));
        assert!(out["resolved"].is_null(), "listing has no resolved count");

        let out = call_ok(
            &client,
            "conflicts",
            serde_json::json!({
                "path": abs(&ws, "merged.rs"),
                "resolve": "@theirs",
            }),
        )
        .await;
        assert_eq!(out["resolved"], 1);
        assert_eq!(
            fs::read_to_string(ws.path().join("merged.rs")).unwrap(),
            "top\ntheirs\nbottom\n"
        );

        let err = call_err(
            &client,
            "conflicts",
            serde_json::json!({"resolve": "@nope"}),
        )
        .await;
        assert!(err.contains("unknown resolve choice"), "{err}");

        drop(client);
        server.abort();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn inspect_finds_todos() {
        let ws = workspace();
        let (client, server) = client().await;

        let out = call_ok(
            &client,
            "inspect",
            serde_json::json!({
                "sections": "todos",
                "scope": abs(&ws, "src/app.py"),
            }),
        )
        .await;
        let text = out["text"].as_str().unwrap();
        assert!(text.contains("(1 items)"), "got {text}");
        assert!(text.contains("handle errors"));

        drop(client);
        server.abort();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn callgraph_over_the_wire() {
        let ws = workspace();
        let (client, server) = client().await;

        let out = call_ok(
            &client,
            "callgraph",
            serde_json::json!({
                "op": "call_tree",
                "path": abs(&ws, "src/main.rs"),
                "symbol": "main",
            }),
        )
        .await;
        let text = out["text"].as_str().unwrap();
        assert!(text.contains("main (line 1)"), "got {text}");
        assert!(text.contains("helper"), "got {text}");

        let out = call_ok(
            &client,
            "callgraph",
            serde_json::json!({
                "op": "callers",
                "path": abs(&ws, "src/main.rs"),
                "symbol": "helper",
            }),
        )
        .await;
        assert!(
            out["text"]
                .as_str()
                .unwrap()
                .contains("callers of \"helper\""),
            "got {out}"
        );

        let out = call_ok(
            &client,
            "callgraph",
            serde_json::json!({
                "op": "impact",
                "path": abs(&ws, "src/main.rs"),
                "symbol": "helper",
            }),
        )
        .await;
        assert!(out["text"].as_str().unwrap().contains("main"), "got {out}");

        let err = call_err(
            &client,
            "callgraph",
            serde_json::json!({
                "op": "impact",
                "path": abs(&ws, "notes.txt"),
                "symbol": "x",
            }),
        )
        .await;
        assert!(err.contains("unsupported file type"), "{err}");

        drop(client);
        server.abort();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn repomap_renders_and_caches() {
        let ws = workspace();
        let (client, server) = client().await;

        let out = call_ok(
            &client,
            "repomap",
            serde_json::json!({
                "path": abs(&ws, "src"),
                "query": "show me the helper function",
            }),
        )
        .await;
        let text = out["text"].as_str().unwrap();
        assert!(text.contains("main.rs"), "got {text}");
        assert!(text.contains("helper"), "got {text}");

        // Same call again hits the per-root cache: identical output.
        let again = call_ok(
            &client,
            "repomap",
            serde_json::json!({
                "path": abs(&ws, "src"),
                "query": "show me the helper function",
            }),
        )
        .await;
        assert_eq!(out["text"], again["text"]);

        drop(client);
        server.abort();
    }
}
