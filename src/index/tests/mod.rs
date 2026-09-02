//! Test doubles and fixtures for the index: `MockEmbedder` + `MemStore`
//! prove the provider/store traits with no model and no SQLite. `pub(crate)`
//! so other modules' tests (sqlite, mcp) reuse the same doubles.

mod reconcile;
mod search;

use std::cell::Cell;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

use tempfile::TempDir;

use super::*;
use crate::concept::Concept;
use crate::context::ProjectContext;
use crate::error::Error;

// --- Test doubles: MockEmbedder + MemStore prove trait sufficiency with
// no model and no SQLite. ---

/// Deterministic provider: every text maps to a normalized 128-dim
/// vector by hashing its tokens into dims, so identical texts always
/// score 1.0 against each other and overlapping texts score positively.
// `pub(crate)` so the default backend's tests (`sqlite.rs`) can use
// the same double against a real store.
pub(crate) struct MockEmbedder {
    model_id: String,
    dimension: usize,
    embed_calls: Cell<usize>,
}

impl MockEmbedder {
    pub(crate) fn new() -> Self {
        Self::with_model_id("mock-embedder@1")
    }

    /// A provider with a different identity, to simulate model flips.
    pub(crate) fn with_model_id(model_id: &str) -> Self {
        Self {
            model_id: model_id.to_string(),
            dimension: 128,
            embed_calls: Cell::new(0),
        }
    }

    pub(crate) fn embed_calls(&self) -> usize {
        self.embed_calls.get()
    }
}

impl EmbeddingProvider for MockEmbedder {
    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn dimensions(&self) -> usize {
        self.dimension
    }

    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.embed_calls.set(self.embed_calls.get() + texts.len());
        Ok(texts
            .iter()
            .map(|text| {
                let mut v = vec![0.0f32; self.dimension];
                for token in text.split_whitespace() {
                    // Word-like normalization (lowercase, punctuation
                    // stripped) so prose tokens match query tokens; then
                    // FNV-1a over the token, folded into one dim with a
                    // deterministic sign and magnitude.
                    let token = token
                        .trim_matches(|c: char| !c.is_alphanumeric())
                        .to_lowercase();
                    if token.is_empty() {
                        continue;
                    }
                    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
                    for byte in token.bytes() {
                        h ^= u64::from(byte);
                        h = h.wrapping_mul(0x0000_0100_0000_01b3);
                    }
                    let dim = (h as usize) % self.dimension;
                    let sign = if h >> 63 == 0 { 1.0 } else { -1.0 };
                    let magnitude = 0.5 + ((h >> 22) as f32 / (u64::MAX >> 22) as f32);
                    v[dim] += sign * magnitude;
                }
                let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
                if norm > 0.0 {
                    for x in &mut v {
                        *x /= norm;
                    }
                }
                v
            })
            .collect())
    }
}

/// HashMap-backed store with brute-force cosine search honoring every
/// `Filter` field, plus bookkeeping (`clears`, `removals`) for
/// assertions. Implements the filter-then-truncate contract verbatim —
/// the reference semantics the sqlite backend must match.
pub(crate) struct MemStore {
    model_id: Option<String>,
    units: HashMap<QualifiedConceptId, EmbeddingUnit>,
    clears: usize,
    removals: Vec<QualifiedConceptId>,
}

impl MemStore {
    pub(crate) fn new() -> Self {
        Self {
            model_id: None,
            units: HashMap::new(),
            clears: 0,
            removals: Vec::new(),
        }
    }
}

impl VectorStore for MemStore {
    fn model_id(&self) -> Option<&str> {
        self.model_id.as_deref()
    }

    fn set_model_id(&mut self, id: &str) -> Result<()> {
        self.model_id = Some(id.to_string());
        Ok(())
    }

    fn upsert(&mut self, units: &[EmbeddingUnit]) -> Result<()> {
        for unit in units {
            self.units.insert(unit.concept.clone(), unit.clone());
        }
        Ok(())
    }

    fn remove_concept(&mut self, concept: &QualifiedConceptId) -> Result<()> {
        self.removals.push(concept.clone());
        self.units.remove(concept);
        Ok(())
    }

    fn unit_hashes(&self) -> Result<HashMap<QualifiedConceptId, String>> {
        Ok(self
            .units
            .iter()
            .map(|(qid, unit)| (qid.clone(), unit.text_hash.clone()))
            .collect())
    }

    fn clear(&mut self) -> Result<()> {
        self.units.clear();
        self.model_id = None;
        self.clears += 1;
        Ok(())
    }

    fn search(&self, vector: &[f32], k: usize, filter: &Filter) -> Result<Vec<SearchHit>> {
        let cosine = |a: &[f32], b: &[f32]| -> f32 {
            let (mut dot, mut na, mut nb) = (0.0f32, 0.0f32, 0.0f32);
            for (x, y) in a.iter().zip(b) {
                dot += x * y;
                na += x * x;
                nb += y * y;
            }
            if na == 0.0 || nb == 0.0 {
                0.0
            } else {
                dot / (na.sqrt() * nb.sqrt())
            }
        };
        let mut hits: Vec<SearchHit> = self
            .units
            .values()
            .filter(|unit| filter_matches(unit, filter))
            .map(|unit| SearchHit {
                concept: unit.concept.clone(),
                score: cosine(vector, &unit.vector),
                meta: unit.meta.clone(),
            })
            .collect();
        // Descending similarity. Ties keep whatever order the
        // filter walk produced — never an argosy-precedence order,
        // because the store never sees precedence.
        hits.sort_by(|a, b| b.score.total_cmp(&a.score));
        hits.truncate(k);
        Ok(hits)
    }
}

fn filter_matches(unit: &EmbeddingUnit, filter: &Filter) -> bool {
    if let Some(namespaces) = &filter.namespaces
        && !namespaces.contains(&unit.concept.namespace)
    {
        return false;
    }
    if let Some(argosies) = &filter.argosies
        && !argosies.contains(&unit.concept.argosy)
    {
        return false;
    }
    if let Some(types) = &filter.concept_types
        && !types
            .iter()
            .any(|t| unit.meta.concept_type.as_deref() == Some(t.as_str()))
    {
        return false;
    }
    if let Some(tags) = &filter.tags
        && !tags.iter().any(|t| unit.meta.tags.contains(t))
    {
        return false;
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

// --- Fixtures ---

/// Writes a minimal openable argosy (manifest + the given files) into a
/// fresh tempdir. `files` are `(bundle-relative path, file content)`.
pub(crate) fn make_argosy(name: &str, files: &[(&str, &str)]) -> TempDir {
    let dir = TempDir::new().unwrap();
    let manifest = format!(
        "---\ntype: Argosy Manifest\nname: {name}\nargosy_version: \"0.3.1\"\n---\n# {name}\n"
    );
    write_file(dir.path(), "argosy.md", &manifest);
    for (rel, content) in files {
        write_file(dir.path(), rel, content);
    }
    dir
}

pub(crate) fn write_file(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

const DOC_ARCH: &str = "---\ntype: Note\ndescription: The architecture.\ntags: [design, rust]\n---\nThe service architecture.\n";
const SKILL_DEPLOY: &str =
    "---\ntype: Skill\ndescription: Deploy the service.\n---\nDeploy steps.\n";
const MEMORY_GOTCHAS: &str =
    "---\ntype: Note\ndescription: Build gotchas.\ntags: [build]\n---\nCargo needs a lockfile.\n";
const RULE_CASE: &str = "---\ntype: Styleguide Rule\ndescription: Naming case.\nlanguage: rust\ncategory: naming\ntags: [style]\n---\n## Good\n```\nfoo\n```\n";
const DOC_LOCKING: &str = "---\ntype: Note\ndescription: Database locking.\ntags: [database]\n---\nLock ordering and retries.\n";

/// The standard fixture: a local argosy with one concept per default
/// namespace, plus an imported one adding a document and — deliberately —
/// a `memory/` entry that the default walk must skip.
pub(crate) fn fixture() -> (TempDir, TempDir, ProjectContext) {
    let local = make_argosy(
        "local",
        &[
            ("document/arch.md", DOC_ARCH),
            ("skill/deploy.md", SKILL_DEPLOY),
            ("memory/gotchas.md", MEMORY_GOTCHAS),
            ("styleguide/rust/naming/case.md", RULE_CASE),
        ],
    );
    let imported = make_argosy(
        "vendor",
        &[
            ("document/locking.md", DOC_LOCKING),
            ("memory/vendor-notes.md", MEMORY_GOTCHAS),
        ],
    );
    let ctx = ProjectContext::open(local.path(), [imported.path().to_path_buf()]).unwrap();
    (local, imported, ctx)
}

fn fresh_index() -> Index<MockEmbedder, MemStore> {
    Index::new(MockEmbedder::new(), MemStore::new())
}
