//! The semantic index: trait boundaries, embedding units, reconciliation, and ranked search.
//!
//! Search is hybrid: vector and lexical legs fused by reciprocal rank fusion ([`rrf_fuse`]).

use std::collections::HashMap;

use crate::bundle::{Argosy, Namespace};
use crate::concept::Concept;
use crate::context::{ProjectContext, QualifiedConceptId};
use crate::error::{IndexSnafu, Result, UnknownArgosySnafu};
use crate::hash::sha256_hex;

pub mod chunk;

pub use chunk::{Chunk, ChunkConfig};

#[cfg(feature = "default-index")]
pub mod sqlite;
#[cfg(feature = "default-index")]
pub mod tract;

/// Produces embedding vectors for texts.
pub trait EmbeddingProvider {
    /// The model's stable identity, e.g. `"tract/all-MiniLM-L6-v2@tract-1"`.
    ///
    /// Must change with the weights: vectors from different models are not
    /// comparable.
    fn model_id(&self) -> &str;

    /// The vector dimensionality every [`EmbeddingProvider::embed`] call produces.
    fn dimensions(&self) -> usize;

    /// Embeds every text in `texts`, returning exactly one vector per text, in order.
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;

    /// Embeds query-side text (the [`Index::search`] input).
    ///
    /// Asymmetric models need a query-side instruction that never appears on
    /// the document side.
    fn embed_query(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.embed(texts)
    }
}

/// The model identity the zero-model embedder ([`LexicalOnlyProvider`]) records.
///
/// Switching to or from a real model is an ordinary identity-mismatch full rebuild.
pub const LEXICAL_ONLY_MODEL_ID: &str = "lexical-only@1";

/// The zero-model [`EmbeddingProvider`] for the `index.search_mode = "lexical"` mode.
///
/// BM25-only retrieval: exact identifiers and rule IDs match strongly, paraphrases do not.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LexicalOnlyProvider;

impl EmbeddingProvider for LexicalOnlyProvider {
    fn model_id(&self) -> &str {
        LEXICAL_ONLY_MODEL_ID
    }

    /// Always zero: the signal to the store that units have no vectors.
    fn dimensions(&self) -> usize {
        0
    }

    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|_| Vec::new()).collect())
    }
}

/// The OKF frontmatter fields a query can filter on.
///
/// A `None` field means undeclared, and such a unit never matches a filter constraining it.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct UnitMeta {
    /// The frontmatter `type` (e.g. `Skill`, `Styleguide Rule`).
    pub concept_type: Option<String>,
    /// The frontmatter `description`.
    pub description: Option<String>,
    /// The frontmatter `tags`.
    pub tags: Vec<String>,
    /// The frontmatter `language` facet (styleguide rules).
    pub language: Option<String>,
    /// The frontmatter `category` facet (styleguide rules).
    pub category: Option<String>,
}

impl UnitMeta {
    /// Reads the filter facets off a parsed concept's frontmatter.
    fn from_concept(concept: &Concept) -> Self {
        Self {
            concept_type: concept.concept_type().map(str::to_string),
            description: concept.description().map(str::to_string),
            tags: concept.tags().iter().map(|s| (*s).to_string()).collect(),
            language: concept.get_str("language").map(str::to_string),
            category: concept.get_str("category").map(str::to_string),
        }
    }
}

/// One embedded unit of a concept: the whole concept, or a heading-based
/// passage ([`chunk::chunk_concept`]).
#[derive(Debug, Clone)]
pub struct EmbeddingUnit {
    /// The source concept's qualified identity.
    pub concept: QualifiedConceptId,
    /// The unit's position within its source concept (`0` for the first
    /// passage).
    pub chunk_ordinal: u32,
    /// The lowercase hex SHA-256 of the embedded text; same text ⇒ no re-embed.
    pub text_hash: String,
    /// The unit's embedding vector.
    pub vector: Vec<f32>,
    /// The filterable facets of the source concept.
    pub meta: UnitMeta,
    /// The passage's heading breadcrumb; empty for a whole-concept chunk or pre-heading passage.
    pub heading_path: String,
    /// 1-based first body line of the passage, frontmatter excluded; the range `read` accepts.
    pub start_line: u32,
    /// 1-based last body line of the passage, frontmatter excluded.
    pub end_line: u32,
    /// The passage as embedded (prefix included, invisible characters stripped).
    pub passage: String,
}

/// The structured constraints a search composes with its query text.
/// Every field is optional; `None` means unconstrained.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    /// Only return units in these namespaces.
    pub namespaces: Option<Vec<Namespace>>,
    /// Only return units from these argosies (manifest names); stores may assume validity.
    pub argosies: Option<Vec<String>>,
    /// Only return units whose `type` is one of these.
    pub concept_types: Option<Vec<String>>,
    /// Only return units carrying at least one of these tags.
    pub tags: Option<Vec<String>>,
    /// Only return units with this exact `language` facet.
    pub language: Option<String>,
    /// Only return units with this exact `category` facet.
    pub category: Option<String>,
}

/// One ranked search result.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SearchHit {
    /// The retrieved concept's qualified identity — origin argosy visible.
    pub concept: QualifiedConceptId,
    /// Reciprocal-rank-fusion score; higher is better, hits sort by it descending.
    pub score: f32,
    /// The unit's facets, so callers can filter/present without resolving.
    pub meta: UnitMeta,
    /// The matched passage's position within its concept.
    pub chunk_ordinal: u32,
    /// The passage's heading breadcrumb; empty for a whole-concept chunk or pre-heading passage.
    pub heading_path: String,
    /// 1-based first body line of the matched passage.
    pub start_line: u32,
    /// 1-based last body line of the matched passage.
    pub end_line: u32,
    /// The matched passage as embedded (prefix included, invisible characters stripped).
    pub passage: String,
}

/// One semantic query.
#[derive(Debug, Clone)]
pub struct Query {
    /// The natural-language query text.
    pub text: String,
    /// Maximum number of hits to return.
    pub k: usize,
    /// Structured narrowing composed with the semantic ranking.
    pub filter: Filter,
}

impl Query {
    /// An unscoped query: searches every active argosy and namespace the store holds.
    pub fn unscoped(text: impl Into<String>, k: usize) -> Self {
        Self {
            text: text.into(),
            k,
            filter: Filter::default(),
        }
    }
}

/// Reciprocal-rank-fusion rank smoothing constant; 60 is the standard default.
pub const RRF_K: u32 = 60;

/// Fuses ranked retrieval legs with reciprocal rank fusion, truncating the fused order to `k`.
///
/// Ties inside a leg share that leg's rank, so fusion never orders results the legs did not.
pub fn rrf_fuse(legs: &[Vec<SearchHit>], k: usize) -> Vec<SearchHit> {
    if legs.is_empty() || k == 0 {
        return Vec::new();
    }
    let mut hits: Vec<SearchHit> = Vec::new();
    let mut fused: Vec<f64> = Vec::new();
    let mut seen: HashMap<(QualifiedConceptId, u32), usize> = HashMap::new();

    for leg in legs {
        let mut rank: u32 = 0;
        let mut prev_score: Option<f32> = None;
        for (position, hit) in leg.iter().enumerate() {
            // Tie-aware ranking: an equal leg score reuses the predecessor's rank.
            if prev_score.is_none_or(|score| hit.score != score) {
                rank = position as u32;
            }
            prev_score = Some(hit.score);
            let contribution = 1.0 / (f64::from(RRF_K) + f64::from(rank));
            let key = (hit.concept.clone(), hit.chunk_ordinal);
            match seen.get(&key) {
                Some(&index) => fused[index] += contribution,
                None => {
                    seen.insert(key, hits.len());
                    fused.push(contribution);
                    hits.push(hit.clone());
                }
            }
        }
    }

    // Stable sort keeps ties in best-leg-first order, never argosy precedence.
    let mut fused_hits: Vec<(f64, SearchHit)> = fused
        .into_iter()
        .zip(hits)
        .map(|(score, mut hit)| {
            hit.score = score as f32;
            (score, hit)
        })
        .collect();
    fused_hits.sort_by(|a, b| b.0.total_cmp(&a.0));
    fused_hits.truncate(k);
    fused_hits.into_iter().map(|(_, hit)| hit).collect()
}

/// Stores and retrieves embedding units; search returns hits ordered by descending similarity.
pub trait VectorStore {
    /// The model identity recorded for the store's current vectors, or `None`
    /// before the first reconcile and after [`VectorStore::clear`].
    fn model_id(&self) -> Option<&str>;

    /// The vector width the store's contents were built with, or `None` when unrecorded.
    fn recorded_dimensions(&self) -> Option<usize> {
        None
    }

    /// Records the model identity of the store's current contents.
    fn set_model_id(&mut self, id: &str) -> Result<()>;

    /// Inserts or replaces units keyed by their source concept; re-upserting must not duplicate.
    fn upsert(&mut self, units: &[EmbeddingUnit]) -> Result<()>;

    /// Drops every unit of one concept (incremental deletion).
    fn remove_concept(&mut self, concept: &QualifiedConceptId) -> Result<()>;

    /// The stored `text_hash` of every concept, the diff input for [`Index::reconcile`].
    fn unit_hashes(&self) -> Result<HashMap<QualifiedConceptId, String>>;

    /// Drops every unit — the full-rebuild path.
    ///
    /// Dropping the recorded model identity is recommended but optional (reconcile re-records it).
    fn clear(&mut self) -> Result<()>;

    /// The `k` units most similar to `vector` under `filter`, ordered by descending similarity.
    ///
    /// Filters constrain the full ranked order; truncate-first backends
    /// over-fetch or lose recall.
    fn search(&self, vector: &[f32], k: usize, filter: &Filter) -> Result<Vec<SearchHit>>;

    /// The lexical (BM25) leg of hybrid retrieval, ordered best-first.
    ///
    /// Honors the same filter-then-truncate contract as [`VectorStore::search`];
    /// defaults to an empty leg, keeping plain vector ranking.
    fn lexical_search(&self, text: &str, k: usize, filter: &Filter) -> Result<Vec<SearchHit>> {
        let _ = (text, k, filter);
        Ok(Vec::new())
    }
}

/// The report of one [`Index::reconcile`] run, printed by the CLI's `index` subcommand.
#[derive(Debug, Clone, serde::Serialize)]
pub struct IndexReport {
    /// True iff the store was cleared and fully re-embedded.
    pub rebuilt: bool,
    /// Concepts embedded and upserted (new or content-changed).
    pub upserted: usize,
    /// Passages (embedding units) the upserted concepts expanded to.
    #[serde(default)]
    pub passages: usize,
    /// Concepts removed from the store (deleted from disk).
    pub removed: usize,
    /// Concepts whose content hash was unchanged.
    pub unchanged: usize,
    /// The model identity the store now records.
    pub model_id: String,
    /// Warnings for invisible format characters stripped from (re)embedded
    /// concepts (see [`is_invisible_format`]); bytes on disk stay unchanged.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// The default namespace set: `document`, `skill`, and `styleguide` of every
/// active argosy, plus `memory`.
fn default_namespaces() -> Vec<Namespace> {
    vec![
        Namespace::Document,
        Namespace::Skill,
        Namespace::Styleguide,
        Namespace::Memory,
    ]
}

/// Walks `namespaces` of every active argosy and hashes each concept's
/// would-be embedded text, sorted by URI for deterministic embedding batches.
///
/// Shared by [`Index::reconcile`] and [`staleness_report`], which must never drift apart.
fn gather_concepts(
    context: &ProjectContext,
    namespaces: &[Namespace],
    include_imported_memory: bool,
    chunk_config: ChunkConfig,
) -> Result<Vec<GatheredConcept>> {
    let mut out = Vec::new();
    let mut visit = |name: &str, argosy: &Argosy, is_local: bool| -> Result<()> {
        for namespace in namespaces {
            if !is_local && !include_imported_memory && *namespace == Namespace::Memory {
                continue;
            }
            for (id, concept) in argosy.concepts(namespace)? {
                let fallback = id
                    .as_str()
                    .rsplit('/')
                    .next()
                    .unwrap_or_default()
                    .to_string();
                let chunks = chunk::chunk_concept(&concept, &fallback, chunk_config);
                out.push(GatheredConcept {
                    qid: QualifiedConceptId {
                        argosy: name.to_string(),
                        namespace: namespace.clone(),
                        id,
                    },
                    hash: concept_hash(&chunks),
                    invisible: chunks.iter().map(|c| c.invisible).sum(),
                    chunks,
                    meta: UnitMeta::from_concept(&concept),
                });
            }
        }
        Ok(())
    };
    let local: &Argosy = context.local();
    visit(local.manifest().name(), local, true)?;
    for argosy in context.imported() {
        visit(argosy.manifest().name(), argosy, false)?;
    }
    out.sort_by_key(|a| a.qid.to_uri());
    Ok(out)
}

/// The concept-level staleness hash: SHA-256 over the chunks' texts,
/// length-delimited so no two chunkings collide.
pub(crate) fn concept_hash(chunks: &[Chunk]) -> String {
    let mut buffer = Vec::new();
    for chunk in chunks {
        buffer.extend_from_slice(&(chunk.text.len() as u64).to_le_bytes());
        buffer.extend_from_slice(chunk.text.as_bytes());
    }
    sha256_hex(&buffer)
}

/// The diff [`staleness_report`] computes: what [`Index::reconcile`] would do, by category.
#[derive(Debug, Clone, serde::Serialize)]
pub struct StalenessReport {
    /// Concepts on disk the store does not hold.
    pub added: usize,
    /// Concepts whose content hash changed since they were embedded.
    pub changed: usize,
    /// Concepts the store holds that no longer exist on disk.
    pub removed: usize,
    /// Concepts whose content hash is unchanged.
    pub unchanged: usize,
    /// True iff a model mismatch forces a full rebuild instead of a diff.
    pub model_mismatch: bool,
}

/// A read-only preview of what [`Index::reconcile`] would apply: no embed calls, no writes.
pub fn staleness_report(
    context: &ProjectContext,
    store: &impl VectorStore,
    expected_model_id: &str,
    chunk_config: ChunkConfig,
) -> Result<StalenessReport> {
    let current = gather_concepts(context, &default_namespaces(), false, chunk_config)?;
    let stored = store.unit_hashes()?;

    // Mirrors reconcile's identity check: data with no recorded identity is a
    // full rebuild, so the preview must not claim "up to date".
    let model_mismatch = match store.model_id() {
        Some(recorded) => recorded != expected_model_id,
        None => !stored.is_empty(),
    };
    let mut report = StalenessReport {
        added: 0,
        changed: 0,
        removed: 0,
        unchanged: 0,
        model_mismatch,
    };
    let on_disk: std::collections::HashSet<&QualifiedConceptId> =
        current.iter().map(|c| &c.qid).collect();
    for qid in stored.keys() {
        if !on_disk.contains(qid) {
            report.removed += 1;
        }
    }
    for gathered in &current {
        match stored.get(&gathered.qid) {
            Some(hash) if *hash == gathered.hash => report.unchanged += 1,
            Some(_) => report.changed += 1,
            None => report.added += 1,
        }
    }
    Ok(report)
}

/// A concept gathered from disk, pre-hash, on its way through reconcile.
struct GatheredConcept {
    qid: QualifiedConceptId,
    /// The concept's chunks, ordered by ordinal.
    chunks: Vec<Chunk>,
    /// The concept-level hash over all chunk texts ([`concept_hash`]); stored
    /// on every unit row.
    hash: String,
    meta: UnitMeta,
    /// Invisible format characters stripped across the chunks (see [`is_invisible_format`]).
    invisible: usize,
}

/// The per-leg candidate depth hybrid search fetches before fusing: several
/// times `k` with a floor, so fusion sees past each leg's top handful.
fn hybrid_fetch_k(k: usize) -> usize {
    (k * 4).max(32)
}

/// The reconcile/query engine over the [`EmbeddingProvider`] and [`VectorStore`] trait boundaries.
pub struct Index<P: EmbeddingProvider, S: VectorStore> {
    provider: P,
    store: S,
    namespaces: Vec<Namespace>,
    include_imported_memory: bool,
    chunk_config: ChunkConfig,
}

impl<P: EmbeddingProvider, S: VectorStore> Index<P, S> {
    /// An index over the default namespace set: `document`, `skill`, and `styleguide`
    /// of every active argosy, plus `memory` of the *local* argosy only.
    ///
    /// Custom namespaces are not indexed by default; use [`Index::with_namespaces`].
    pub fn new(provider: P, store: S) -> Self {
        Self {
            provider,
            store,
            namespaces: default_namespaces(),
            include_imported_memory: false,
            chunk_config: ChunkConfig::default(),
        }
    }

    /// An index over an explicit namespace set, honored verbatim — including
    /// `memory` of imports when listed.
    ///
    /// Reconcile removes anything stored that the selection no longer walks.
    pub fn with_namespaces(provider: P, store: S, namespaces: Vec<Namespace>) -> Self {
        Self {
            provider,
            store,
            namespaces,
            include_imported_memory: true,
            chunk_config: ChunkConfig::default(),
        }
    }

    /// Overrides the chunking budget (user configuration `index.chunk_max_tokens`).
    ///
    /// A different budget re-chunks and re-hashes affected concepts, so the next
    /// reconcile re-embeds them.
    pub fn with_chunk_config(mut self, chunk_config: ChunkConfig) -> Self {
        self.chunk_config = chunk_config;
        self
    }

    /// The chunking budget this index reconciles with.
    pub fn chunk_config(&self) -> ChunkConfig {
        self.chunk_config
    }

    /// The embedding provider.
    pub fn provider(&self) -> &P {
        &self.provider
    }

    /// Replaces the provider; the next reconcile rebuilds on the model identity change.
    pub fn set_provider(&mut self, provider: P) {
        self.provider = provider;
    }

    /// The vector store.
    pub fn store(&self) -> &S {
        &self.store
    }

    /// The vector store, mutably.
    ///
    /// Bypassing reconcile can desync the store's hashes from the bundles — prefer reconcile.
    pub fn store_mut(&mut self) -> &mut S {
        &mut self.store
    }

    /// The namespaces this index walks.
    pub fn namespaces(&self) -> &[Namespace] {
        &self.namespaces
    }

    /// Walks the configured namespaces of every active argosy.
    fn gather(&self, context: &ProjectContext) -> Result<Vec<GatheredConcept>> {
        gather_concepts(
            context,
            &self.namespaces,
            self.include_imported_memory,
            self.chunk_config,
        )
    }

    /// Brings the store in line with the disk: full rebuild on model identity
    /// mismatch, otherwise an incremental diff.
    ///
    /// A mismatch clears the store before re-embedding, so it never holds two models' vectors.
    pub fn reconcile(&mut self, context: &ProjectContext) -> Result<IndexReport> {
        self.reconcile_with_progress(context, |_, _| {})
    }

    /// Like [`Index::reconcile`], reporting `(completed, total)` once the diff
    /// is known and after each committed batch.
    pub fn reconcile_with_progress(
        &mut self,
        context: &ProjectContext,
        mut progress: impl FnMut(usize, usize),
    ) -> Result<IndexReport> {
        let current = self.gather(context)?;
        let stored = self.store.unit_hashes()?;
        let model = self.provider.model_id().to_string();

        let identity_mismatch = match self.store.model_id() {
            Some(recorded) => recorded != model,
            // Data with no recorded identity cannot be trusted — treat it as a mismatch.
            None => !stored.is_empty(),
        };

        let mut report = IndexReport {
            rebuilt: identity_mismatch,
            upserted: 0,
            passages: 0,
            removed: 0,
            unchanged: 0,
            model_id: model.clone(),
            warnings: Vec::new(),
        };

        let prior: HashMap<QualifiedConceptId, String> = if identity_mismatch {
            self.store.clear()?;
            HashMap::new()
        } else {
            stored
        };

        let on_disk: std::collections::HashSet<&QualifiedConceptId> =
            current.iter().map(|c| &c.qid).collect();
        for qid in prior.keys() {
            if !on_disk.contains(qid) {
                self.store.remove_concept(qid)?;
                report.removed += 1;
            }
        }

        let mut to_embed = Vec::new();
        for gathered in &current {
            match prior.get(&gathered.qid) {
                Some(hash) if *hash == gathered.hash => report.unchanged += 1,
                _ => to_embed.push(gathered),
            }
        }

        // Batching bounds the embedding input and vectors; each upsert is atomic,
        // so a later failure leaves completed batches for the next reconcile to resume.
        // Batches are grouped by CONCEPT: the store's upsert replaces a concept's
        // units wholesale, so splitting one across two batches would drop rows.
        const EMBED_BATCH: usize = 32;
        let total: usize = to_embed.iter().map(|g| g.chunks.len()).sum();
        progress(0, total);
        let mut start = 0;
        while start < to_embed.len() {
            let mut end = start;
            let mut units = 0usize;
            while end < to_embed.len()
                && (end == start || units + to_embed[end].chunks.len() <= EMBED_BATCH)
            {
                units += to_embed[end].chunks.len();
                end += 1;
            }
            let batch = &to_embed[start..end];
            let texts: Vec<String> = batch
                .iter()
                .flat_map(|g| g.chunks.iter().map(|c| c.text.clone()))
                .collect();
            let vectors = self.provider.embed(&texts)?;
            if vectors.len() != texts.len() {
                return IndexSnafu {
                    reason: format!(
                        "provider `{}` returned {} vectors for a batch of {} texts",
                        model,
                        vectors.len(),
                        texts.len()
                    ),
                }
                .fail();
            }
            let mut produced = vectors.into_iter();
            let mut new_units = Vec::with_capacity(texts.len());
            for gathered in batch {
                for chunk in &gathered.chunks {
                    let Some(vector) = produced.next() else {
                        return IndexSnafu {
                            reason: "provider returned fewer vectors than chunk texts".to_string(),
                        }
                        .fail();
                    };
                    new_units.push(EmbeddingUnit {
                        concept: gathered.qid.clone(),
                        chunk_ordinal: chunk.ordinal,
                        text_hash: gathered.hash.clone(),
                        vector,
                        meta: gathered.meta.clone(),
                        heading_path: chunk.heading_path.clone(),
                        start_line: chunk.start_line,
                        end_line: chunk.end_line,
                        passage: chunk.text.clone(),
                    });
                }
            }
            self.store.upsert(&new_units)?;
            report.upserted += batch.len();
            report.passages += new_units.len();
            progress(report.passages, total);
            start = end;
        }

        // Invisible-Unicode flagging: RAG-poisoning defense; warn only for
        // concepts this run embedded, so unchanged concepts never re-warn.
        for gathered in &to_embed {
            if gathered.invisible > 0 {
                report.warnings.push(format!(
                    "{}: stripped {} invisible format character(s) (zero-width/bidi/tag \
                     Unicode) from the indexed text; the on-disk file is unchanged",
                    gathered.qid.to_uri(),
                    gathered.invisible
                ));
            }
        }

        // Every rebuild (and first reconcile) explicitly records the identity —
        // clear() is not required to drop it, and a stale identity would force
        // a full re-embed on every run.
        if report.rebuilt || self.store.model_id().is_none() {
            self.store.set_model_id(&model)?;
        }

        Ok(report)
    }

    /// Ranked hybrid search: the store's vector and lexical legs fused by
    /// reciprocal rank fusion ([`rrf_fuse`]).
    ///
    /// Names in `filter.argosies` must be active in `context` — else
    /// [`crate::error::Error::UnknownArgosy`], never a silent empty.
    pub fn search(&self, context: &ProjectContext, query: &Query) -> Result<Vec<SearchHit>> {
        if let Some(reason) = self.dimension_guard_reason() {
            return IndexSnafu { reason }.fail();
        }
        if let Some(argosies) = &query.filter.argosies {
            for name in argosies {
                if context.argosy_named(name).is_none() {
                    return UnknownArgosySnafu { name: name.clone() }.fail();
                }
            }
        }
        let texts = std::slice::from_ref(&query.text);
        let mut vectors = self.provider.embed_query(texts)?;
        let Some(vector) = vectors.pop() else {
            return IndexSnafu {
                reason: format!(
                    "provider `{}` returned no vector for a one-text batch",
                    self.provider.model_id()
                ),
            }
            .fail();
        };
        if !vectors.is_empty() {
            return IndexSnafu {
                reason: format!(
                    "provider `{}` returned {} vectors for a one-text batch",
                    self.provider.model_id(),
                    vectors.len() + 1
                ),
            }
            .fail();
        }
        // Cheap to over-fetch: vec0 already brute-forces every row and the
        // FTS5 leg has one LIMIT.
        let fetch = hybrid_fetch_k(query.k);
        let vector_hits = self.store.search(&vector, fetch, &query.filter)?;
        let lexical_hits = self
            .store
            .lexical_search(&query.text, fetch, &query.filter)?;
        Ok(rrf_fuse(&[vector_hits, lexical_hits], query.k))
    }

    /// The read-path half of a model switch: a search against a stale store
    /// must fail with a rebuild hint, never a backend shape error.
    fn dimension_guard_reason(&self) -> Option<String> {
        match self.store.recorded_dimensions() {
            Some(recorded) if recorded != self.provider.dimensions() => Some(format!(
                "index was built with a different embedding model: store holds {recorded}-dim \
                     vectors, provider `{}` emits {}; run `argosy index build` to rebuild",
                self.provider.model_id(),
                self.provider.dimensions()
            )),
            None if self.store_has_units() => Some(
                "index dimensionality is unrecorded but the index holds vectors; \
                 run `argosy index build` to rebuild"
                    .to_string(),
            ),
            _ => None,
        }
    }

    /// Whether the store holds any unit; a failed probe counts as holding (the
    /// search would surface the same failure anyway).
    fn store_has_units(&self) -> bool {
        self.store
            .unit_hashes()
            .map(|hashes| !hashes.is_empty())
            .unwrap_or(true)
    }
}

/// Unicode that renders as nothing or as pure formatting: the RAG-poisoning steganography channel.
fn is_invisible_format(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'                // soft hyphen
            | '\u{180E}'          // mongolian vowel separator
            | '\u{200B}'..='\u{200F}' // zero-width space/non-joiner/joiner + LRM/RLM
            | '\u{202A}'..='\u{202E}' // bidi embedding/override controls
            | '\u{2060}'..='\u{2064}' // word joiner + invisible operators
            | '\u{2066}'..='\u{206F}' // bidi isolates
            | '\u{FEFF}'          // zero-width no-break space (BOM)
            | '\u{FFF9}'..='\u{FFFB}' // interlinear annotation
            | '\u{E0001}'          // language tag character
            | '\u{E0020}'..='\u{E007F}' // tag characters (ASCII hidden in them)
    )
}

// Invisible format stripping (in `chunk::chunk_concept`) touches only the
// indexed text: bytes on disk stay as written, so the integrity sidecar
// still verifies and planted payloads remain inspectable by `read`.

/// Builds the id callers use for [`ConceptId`] plumbing in tests.
#[cfg(test)]
fn concept_id(id: &str) -> crate::concept::ConceptId {
    id.parse().expect("test fixture ids are valid")
}

#[cfg(test)]
pub(crate) mod tests;
