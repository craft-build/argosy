//! Shared fixtures and backend doubles for the MCP integration tests:
//! a real rmcp client over a real rmcp `ServerHandler`, with local
//! public-trait doubles (the crate's `pub(crate)` doubles are invisible
//! from an integration test).

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use argosy::Namespace;
use argosy::context::{ProjectContext, QualifiedConceptId};
use argosy::index::{EmbeddingProvider, EmbeddingUnit, Filter, Index, SearchHit, VectorStore};
use argosy::mcp::{McpState, ProjectSession, SessionFactory};
use argosy::{Concept, LocalArgosy, Result};
use tempfile::TempDir;

// --- Public-trait backend doubles (no model, no SQLite) -------------------

pub(crate) const DIM: usize = 64;

/// Deterministic token-bucket embedder: normalized tokens hash into
/// dimensions, so overlapping texts score positively under cosine, like the
/// crate's `MockEmbedder` (duplicated here because test doubles are
/// `pub(crate)`).
pub(crate) struct FakeEmbedder;

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
pub(crate) struct MemVec {
    model_id: Option<String>,
    units: HashMap<(QualifiedConceptId, u32), EmbeddingUnit>,
}

pub(crate) fn matches_filter(unit: &EmbeddingUnit, filter: &Filter) -> bool {
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
pub(crate) fn fixture_copy(name: &str) -> TempDir {
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
pub(crate) fn import_fixture() -> TempDir {
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

pub(crate) struct Rig {
    pub(crate) _local: TempDir,
    pub(crate) _imported: TempDir,
    /// The cwd tool calls name: the local fixture's path (the factory maps
    /// any root to the fixture session, so this is a stand-in root).
    pub(crate) cwd: PathBuf,
    pub(crate) state: McpState<FakeEmbedder, MemVec>,
}

pub(crate) fn rig() -> Rig {
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

// --- Call helpers shared by the wire tests ---

pub(crate) fn call(name: &str, arguments: serde_json::Value) -> rmcp::model::CallToolRequestParams {
    let mut params = rmcp::model::CallToolRequestParams::new(name.to_string());
    params.arguments = Some(arguments.as_object().expect("object args").clone());
    params
}

/// Unwraps a completed tool result; MRTR task/input outcomes are never
/// emitted by this server.
pub(crate) fn complete(response: rmcp::model::CallToolResponse) -> rmcp::model::CallToolResult {
    match response {
        rmcp::model::CallToolResponse::Complete(result) => result,
        other => panic!("expected a complete result, got {other:?}"),
    }
}

pub(crate) fn structured(result: &rmcp::model::CallToolResult) -> serde_json::Value {
    result
        .structured_content
        .clone()
        .expect("structured output present")
}
