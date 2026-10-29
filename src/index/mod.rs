//! The semantic index: trait boundaries, embedding units, reconciliation,
//! and ranked search. The index is a derived, rebuildable artifact whose
//! only inputs are on-disk bundles and an [`EmbeddingProvider`]; custom
//! backends implement the traits, sqlite-vec + tract by default. One or
//! more units per concept — a short concept is a single unit, a long one is
//! split into heading-based passages ([`chunk`]) — and every hit names the
//! passage it matched. Search is hybrid: the store's vector (k-NN) and
//! lexical (BM25) legs are fused by reciprocal rank fusion
//! ([`rrf_fuse`]), so exact identifiers rank next to semantic matches;
//! a store with no lexical leg (the default trait method) keeps plain
//! vector ranking. A zero-model [`LexicalOnlyProvider`] powers the
//! lexical-only mode. Search via [`Index::search`], lookup via resolve.

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

/// Produces embedding vectors for texts. Batch-only: every call, even for a
/// single text, is a `&[String]` batch returning one vector per text in the
/// same order — this keeps the trait minimal and lets backends batch
/// efficiently.
pub trait EmbeddingProvider {
    /// The stable identity of the model — name and version/revision
    /// (e.g. `"tract/all-MiniLM-L6-v2@tract-1"`). Compared against the
    /// store's recorded identity on every reconcile: vectors from different
    /// models are not comparable, so this must change with the weights.
    fn model_id(&self) -> &str;

    /// The vector dimensionality every [`EmbeddingProvider::embed`] call
    /// produces.
    fn dimensions(&self) -> usize;

    /// Embeds every text in `texts`, returning exactly one vector of
    /// [`EmbeddingProvider::dimensions`] floats per text, in order.
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;

    /// Embeds query-side text (the [`Index::search`] input). Asymmetric
    /// models (the BGE family) require a query-side instruction that must
    /// never appear on the document side ([`EmbeddingProvider::embed`],
    /// used at index time); symmetric models default to plain `embed`.
    fn embed_query(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.embed(texts)
    }
}

/// The model identity the zero-model embedder records
/// ([`LexicalOnlyProvider`]): a store built in lexical mode records it
/// like any other model identity, so switching to or from a real model is
/// an ordinary identity-mismatch full rebuild.
pub const LEXICAL_ONLY_MODEL_ID: &str = "lexical-only@1";

/// The zero-model [`EmbeddingProvider`]: the `index.search_mode =
/// "lexical"` mode. Chunking, hashing, staleness diffing, and BM25 stay
/// exactly the hybrid path's, but no vector is ever produced and no model
/// is ever downloaded — the store records zero dimensionality (no vec0
/// table at all) and search ranks by the lexical leg alone. This is
/// BM25-only retrieval: exact identifiers and rule IDs match strongly,
/// paraphrases do not.
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

/// The structured metadata a query can filter on:
/// the OKF frontmatter fields, flattened and nullable to mirror the optional
/// accessors on [`Concept`]. A `None` field means the source concept did not
/// declare it, and such a unit cannot match a filter constraining that field.
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

/// One embedded unit of a concept: enough to trace any retrieval
/// back to its source concept unambiguously. A concept is one unit when it
/// fits the chunk budget, or one unit per heading-based passage when it does
/// not ([`chunk::chunk_concept`]), distinguished by
/// [`EmbeddingUnit::chunk_ordinal`].
#[derive(Debug, Clone)]
pub struct EmbeddingUnit {
    /// The source concept's qualified identity.
    pub concept: QualifiedConceptId,
    /// The unit's position within its source concept (`0` for the first
    /// passage).
    pub chunk_ordinal: u32,
    /// The lowercase hex SHA-256 of the exact text that was embedded. Doubles
    /// as the staleness signal and the content-change detector
    ///: same text ⇒ same hash ⇒ no re-embed.
    pub text_hash: String,
    /// The unit's embedding vector.
    pub vector: Vec<f32>,
    /// The filterable facets of the source concept.
    pub meta: UnitMeta,
    /// The passage's heading breadcrumb (`"Setup › Database"`); empty for a
    /// whole-concept chunk or a pre-heading passage.
    pub heading_path: String,
    /// 1-based first line of the concept body the passage covers
    /// (frontmatter excluded) — the range `read` accepts for expansion.
    pub start_line: u32,
    /// 1-based last line of the concept body the passage covers.
    pub end_line: u32,
    /// The passage text as embedded (prefix included, invisible characters
    /// stripped), returned in hits so an agent can answer without a second
    /// `read`.
    pub passage: String,
}

/// The structured constraints a search composes with its query text.
/// Every field is optional; `None` means unconstrained.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    /// Only return units in these namespaces.
    pub namespaces: Option<Vec<Namespace>>,
    /// Only return units from these argosies (manifest names).
    /// [`Index::search`] validates every name against the context's active
    /// set before reaching the store; individual stores may assume validity.
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
    /// Fused retrieval score: the reciprocal-rank-fusion sum over the
    /// retrieval legs that returned this unit ([`rrf_fuse`]); higher is
    /// better and hits are ordered by descending score. Not a
    /// similarity — leg manifests (cosine, BM25, …) are fused by rank,
    /// never blended by value.
    pub score: f32,
    /// The retrieved unit's facets, so callers can present/filter further
    /// without resolving the concept.
    pub meta: UnitMeta,
    /// The matched passage's position within its concept.
    pub chunk_ordinal: u32,
    /// The passage's heading breadcrumb (`"Setup › Database"`); empty for a
    /// whole-concept chunk or a pre-heading passage.
    pub heading_path: String,
    /// 1-based first body line of the matched passage.
    pub start_line: u32,
    /// 1-based last body line of the matched passage.
    pub end_line: u32,
    /// The matched passage text (prefix included, invisible characters
    /// stripped). Imported passages are delimiter-wrapped at the MCP layer,
    /// like every other imported free text.
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
    /// An unscoped query: searches every active argosy and
    /// namespace the store holds.
    pub fn unscoped(text: impl Into<String>, k: usize) -> Self {
        Self {
            text: text.into(),
            k,
            filter: Filter::default(),
        }
    }
}

/// The reciprocal-rank-fusion smoothing constant: the rank offset that
/// keeps one leg's top hits from dominating the fused order. 60 is the
/// literature default (Cormack et al., 2009 — the same value the hybrid
/// retrieval literature reports needs no per-corpus tuning), which is
/// why legs are fused by rank instead of blending normalized scores.
pub const RRF_K: u32 = 60;

/// Fuses ranked retrieval legs with reciprocal rank fusion: each unit's
/// fused score is the sum, over the legs that returned it, of
/// `1/(RRF_K + rank)`. Legs are ordered best-first (the vector leg
/// first); a unit is identified by `(concept, chunk_ordinal)`, so one
/// passage returned by both legs scores twice — the intended hybrid
/// boost — while different passages of the same concept compete
/// independently. Result: the fused score ordering, truncated to `k`.
///
/// **Tie preservation:** equal-scored hits inside one leg share that leg's
/// rank (the rank at which the tie group starts, competition-style), so
/// content ties remain fused-score ties and fusion never introduces a
/// distinction the legs' own rankings did not make — retrieval alone
/// orders results, per the spec's no-precedence rule (QRY-7). A single
/// leg therefore keeps its exact order, with scores rescaled to
/// `1/(RRF_K + rank)`.
pub fn rrf_fuse(legs: &[Vec<SearchHit>], k: usize) -> Vec<SearchHit> {
    if legs.is_empty() || k == 0 {
        return Vec::new();
    }
    // First-appearance payload per unit (both legs decode the same units
    // row, so a unit's payload is leg-independent; taking the first
    // avoids clones), the fused-score table parallel to it, and the key
    // index into both.
    let mut hits: Vec<SearchHit> = Vec::new();
    let mut fused: Vec<f64> = Vec::new();
    let mut seen: HashMap<(QualifiedConceptId, u32), usize> = HashMap::new();

    for leg in legs {
        let mut rank: u32 = 0;
        let mut prev_score: Option<f32> = None;
        for (position, hit) in leg.iter().enumerate() {
            // Tie-aware ranking: a hit whose leg score equals its
            // predecessor's reuses the predecessor's rank.
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

    // Fused scores in, stable descending sort out: ties keep
    // first-appearance order, which is best-leg-first — never an
    // argosy-precedence order, because the store never sees precedence.
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

/// Stores and retrieves embedding units. Implementations own the ranking
/// contract: [`VectorStore::search`] returns hits ordered by descending
/// similarity. `argosy` ships a sqlite-vec implementation behind
/// the `default-index` feature; any store honoring this contract composes
/// with [`Index`].
pub trait VectorStore {
    /// The model identity recorded for the vectors the store currently
    /// holds, or `None` before the first reconcile and after
    /// [`VectorStore::clear`].
    fn model_id(&self) -> Option<&str>;

    /// The vector width the store's current contents were built with, or
    /// `None` when the backend does not record it. Search compares it
    /// against the provider's width so a model switch surfaces as an
    /// actionable rebuild hint instead of a backend shape error.
    fn recorded_dimensions(&self) -> Option<usize> {
        None
    }

    /// Records the model identity of the store's current contents (set by
    /// [`Index::reconcile`] after a (re)build).
    fn set_model_id(&mut self, id: &str) -> Result<()>;

    /// Inserts or replaces units keyed by their source concept —
    /// re-upserting must not duplicate it.
    fn upsert(&mut self, units: &[EmbeddingUnit]) -> Result<()>;

    /// Drops every unit of one concept (incremental deletion).
    fn remove_concept(&mut self, concept: &QualifiedConceptId) -> Result<()>;

    /// The stored `text_hash` of every concept — the diff input
    /// [`Index::reconcile`] compares against freshly hashed content
    ///, so unchanged concepts never reach the embedder.
    fn unit_hashes(&self) -> Result<HashMap<QualifiedConceptId, String>>;

    /// Drops every unit — the full-rebuild path. Dropping the
    /// recorded model identity too is recommended but not required:
    /// [`Index::reconcile`] re-records the identity explicitly after every
    /// rebuild rather than relying on this side effect.
    fn clear(&mut self) -> Result<()>;

    /// The `k` units most similar to `vector` under `filter`, ordered by
    /// descending similarity. **Filter contract**: filters constrain the
    /// full ranked order and `k` truncates afterwards — truncating before
    /// filtering silently loses recall when the k nearest units are
    /// non-matching. Backends that truncate first must over-fetch.
    fn search(&self, vector: &[f32], k: usize, filter: &Filter) -> Result<Vec<SearchHit>>;

    /// The lexical (BM25/full-text) leg of hybrid retrieval: the `k`
    /// best-ranked units for `text` under `filter`, ordered best-first.
    /// Only each leg's ORDER matters to the fuse — scores are
    /// backend-specific and comparable within a leg, never across legs.
    /// Honors the same filter-then-truncate contract as
    /// [`VectorStore::search`].
    ///
    /// **Default:** an empty leg, so backends without a lexical index
    /// keep ordinary vector-only ranking; fusing one non-empty leg
    /// preserves its order verbatim.
    fn lexical_search(&self, text: &str, k: usize, filter: &Filter) -> Result<Vec<SearchHit>> {
        let _ = (text, k, filter);
        Ok(Vec::new())
    }
}

/// The report of one [`Index::reconcile`] run, for observability (printed by
/// the CLI's `index` subcommand).
#[derive(Debug, Clone, serde::Serialize)]
pub struct IndexReport {
    /// True iff the store was cleared and fully re-embedded.
    pub rebuilt: bool,
    /// Concepts embedded and upserted (new or content-changed).
    pub upserted: usize,
    /// Passages (embedding units) those concepts expanded to: equal to
    /// `upserted` when every concept fit one chunk, larger when long
    /// concepts were split by heading.
    #[serde(default)]
    pub passages: usize,
    /// Concepts removed from the store (deleted from disk).
    pub removed: usize,
    /// Concepts whose content hash was unchanged — each cost one hash and no
    /// embedding.
    pub unchanged: usize,
    /// The model identity the store now records.
    pub model_id: String,
    /// One line per concept that was (re)embedded carrying invisible
    /// format characters (zero-width/bidi/tag Unicode — see
    /// [`is_invisible_format`]): stripped from the indexed text, bytes on
    /// disk unchanged. Empty for a clean run; an unchanged book never
    /// re-warns, because only units being embedded are reported.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// The default namespace set an [`Index::new`] index walks (and the set
/// [`staleness_report`] diffs against): `document`, `skill`, and
/// `styleguide` of every active argosy, plus `memory`.
fn default_namespaces() -> Vec<Namespace> {
    vec![
        Namespace::Document,
        Namespace::Skill,
        Namespace::Styleguide,
        Namespace::Memory,
    ]
}

/// Walks `namespaces` of every active argosy in `context` and hashes each
/// concept's would-be embedded text. Sorted by URI so downstream embedding
/// batches are deterministic. Shared by [`Index::reconcile`] and
/// [`staleness_report`], which must never drift apart. Each gathered
/// concept carries its chunks (see [`chunk::chunk_concept`]) and their
/// invisible-format character count for reconcile-time warnings.
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

/// The concept-level staleness hash: SHA-256 over every chunk's text, in
/// order, length-delimited so no two chunkings collide. Stored on every
/// unit row of the concept, which is what keeps [`VectorStore::unit_hashes`]
/// (one entry per concept) and the reconcile diff unchanged.
pub(crate) fn concept_hash(chunks: &[Chunk]) -> String {
    let mut buffer = Vec::new();
    for chunk in chunks {
        buffer.extend_from_slice(&(chunk.text.len() as u64).to_le_bytes());
        buffer.extend_from_slice(chunk.text.as_bytes());
    }
    sha256_hex(&buffer)
}

/// The diff [`staleness_report`] computes: what [`Index::reconcile`] would
/// do, by category.
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
    /// True iff the store's recorded model identity differs from
    /// `expected_model_id`, OR the store holds data with no recorded
    /// identity — either way a full rebuild, not an incremental diff
    /// (mirroring [`Index::reconcile`]'s guard).
    pub model_mismatch: bool,
}

/// A read-only, embed-free preview of what [`Index::reconcile`] would apply
/// (the CLI's `index status`): gathers the default namespace
/// selection from disk, chunks and hashes each concept, and diffs against
/// [`VectorStore::unit_hashes`]. Makes no embed calls and no writes.
pub fn staleness_report(
    context: &ProjectContext,
    store: &impl VectorStore,
    expected_model_id: &str,
    chunk_config: ChunkConfig,
) -> Result<StalenessReport> {
    let current = gather_concepts(context, &default_namespaces(), false, chunk_config)?;
    let stored = store.unit_hashes()?;

    // Mirrors reconcile's identity check exactly: a store holding
    // data with no recorded identity is a full rebuild, not a diff — the
    // preview must never claim "up to date" when the next build re-embeds
    // everything.
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
    /// The concept-level hash over all chunk texts (see [`concept_hash`]);
    /// stored on every unit row.
    hash: String,
    meta: UnitMeta,
    /// How many invisible format characters were stripped across the
    /// chunks (see [`is_invisible_format`]); zero for honest text. Reported
    /// as a warning when — and only when — the concept is (re)embedded, so
    /// an unchanged book does not re-warn on every reconcile.
    invisible: usize,
}

/// The per-leg candidate depth hybrid search fetches before fusing:
/// several times the caller's k with a floor, so reciprocal-rank fusion
/// has more to fuse than each leg's top handful, without paying for a
/// corpus-wide ranking on either leg.
fn hybrid_fetch_k(k: usize) -> usize {
    (k * 4).max(32)
}

/// The reconcile/query engine, written against the [`EmbeddingProvider`] and
/// [`VectorStore`] trait boundaries only. Owns the provider and the store.
pub struct Index<P: EmbeddingProvider, S: VectorStore> {
    provider: P,
    store: S,
    namespaces: Vec<Namespace>,
    include_imported_memory: bool,
    chunk_config: ChunkConfig,
}

impl<P: EmbeddingProvider, S: VectorStore> Index<P, S> {
    /// An index over the default namespace set: `document`, `skill`, and
    /// `styleguide` of every active argosy, plus `memory` of the *local*
    /// argosy only. Custom namespaces are not indexed by default; use
    /// [`Index::with_namespaces`]. Chunking uses [`ChunkConfig::default`];
    /// use [`Index::with_chunk_config`] to honor user configuration.
    pub fn new(provider: P, store: S) -> Self {
        Self {
            provider,
            store,
            namespaces: default_namespaces(),
            include_imported_memory: false,
            chunk_config: ChunkConfig::default(),
        }
    }

    /// An index over an explicit namespace set, honored verbatim for every
    /// active argosy — including `memory` of imports when listed. The
    /// selection is part of the index's identity: reconcile removes anything
    /// stored that the current selection no longer walks.
    pub fn with_namespaces(provider: P, store: S, namespaces: Vec<Namespace>) -> Self {
        Self {
            provider,
            store,
            namespaces,
            include_imported_memory: true,
            chunk_config: ChunkConfig::default(),
        }
    }

    /// Overrides the chunking budget (user configuration
    /// `index.chunk_max_tokens`). A different budget re-chunks and re-hashes
    /// affected concepts, so the next reconcile re-embeds them.
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

    /// Replaces the provider (e.g. after a model upgrade). The next
    /// [`Index::reconcile`] detects the identity change and
    /// rebuilds; vectors from the old model are never mixed with the new
    /// one's.
    pub fn set_provider(&mut self, provider: P) {
        self.provider = provider;
    }

    /// The vector store.
    pub fn store(&self) -> &S {
        &self.store
    }

    /// The vector store, mutably. Bypassing [`Index::reconcile`] can desync
    /// the store's hashes from the bundles — prefer reconcile.
    pub fn store_mut(&mut self) -> &mut S {
        &mut self.store
    }

    /// The namespaces this index walks.
    pub fn namespaces(&self) -> &[Namespace] {
        &self.namespaces
    }

    /// Walks the configured namespaces of every active argosy and hashes
    /// each concept's would-be embedded text. Sorted by URI so downstream
    /// embedding batches are deterministic.
    fn gather(&self, context: &ProjectContext) -> Result<Vec<GatheredConcept>> {
        gather_concepts(
            context,
            &self.namespaces,
            self.include_imported_memory,
            self.chunk_config,
        )
    }

    /// Brings the store in line with the disk. An identity mismatch (or
    /// data with none recorded) clears the store **before** re-embedding,
    /// so it never holds two models' vectors. Otherwise a diff: only new or
    /// changed concepts are embedded, gone ones removed — reconcile scales
    /// with what changed. Returns an [`IndexReport`]; fully rebuildable.
    pub fn reconcile(&mut self, context: &ProjectContext) -> Result<IndexReport> {
        self.reconcile_with_progress(context, |_, _| {})
    }

    /// Like [`Index::reconcile`], reporting `(completed, total)` passages
    /// needing embeddings once the diff is known and after each committed
    /// batch. An unchanged run reports `(0, 0)`. The callback is not invoked
    /// for scanning, deletions, or model loading.
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
            // Data with no recorded identity cannot be trusted to match this
            // provider either — treat it as a mismatch.
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

        // Deletions: stored concepts no longer on disk under any walked
        // namespace. The lookup set keeps this O(stored + current)
        // rather than O(stored * current).
        let on_disk: std::collections::HashSet<&QualifiedConceptId> =
            current.iter().map(|c| &c.qid).collect();
        for qid in prior.keys() {
            if !on_disk.contains(qid) {
                self.store.remove_concept(qid)?;
                report.removed += 1;
            }
        }

        // New or content-changed concepts; everything else is one
        // hash and done.
        let mut to_embed = Vec::new();
        for gathered in &current {
            match prior.get(&gathered.qid) {
                Some(hash) if *hash == gathered.hash => report.unchanged += 1,
                _ => to_embed.push(gathered),
            }
        }

        // Keep both the embedding input and the resulting vectors bounded.
        // Each upsert is atomic in the sqlite store; a later failure can leave
        // completed batches in place, which the next reconcile will diff and
        // resume (or rebuild if the model identity was not recorded yet).
        // Batches are grouped by CONCEPT, because the store's upsert replaces
        // a concept's units wholesale — splitting one concept's chunks across
        // two batches would drop the first batch's rows.
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

        // Invisible-Unicode flagging (RAG-poisoning defense): warn for the
        // concepts this run actually embedded, so the report stays quiet
        // once the stripped text is what the store holds.
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

        // First reconcile (no identity yet) and every rebuild record the
        // current identity. The rebuild branch does not rely on
        // `clear()` having dropped the identity: a store that kept it would
        // otherwise reconcile against a stale identity on every run and
        // clear + re-embed the whole corpus each time.
        if report.rebuilt || self.store.model_id().is_none() {
            self.store.set_model_id(&model)?;
        }

        Ok(report)
    }

    /// Ranked hybrid search: embeds [`Query::text`], runs the store's
    /// vector leg on the embedding and its lexical (BM25) leg on the raw
    /// text, and fuses the two rankings by reciprocal rank fusion
    /// ([`rrf_fuse`]) before truncating to [`Query::k`]. A store without
    /// a lexical leg (the trait default) keeps pure vector ranking;
    /// a zero-dimension provider (`LexicalOnlyProvider`, the
    /// `index.search_mode = "lexical"` mode) keeps pure lexical ranking.
    /// A query with no indexable terms falls to the vector leg alone.
    /// For direct lookup use [`ProjectContext::resolve`]. Names in
    /// `filter.argosies` must be active in `context` — else
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
        // Each leg ranks a wider candidate set than the caller's k —
        // cheap on both (vec0 already brute-forces every row; the FTS5
        // leg has one LIMIT) and what lets the fusion see past the top
        // handful — then the fuse truncates to k.
        let fetch = hybrid_fetch_k(query.k);
        let vector_hits = self.store.search(&vector, fetch, &query.filter)?;
        let lexical_hits = self
            .store
            .lexical_search(&query.text, fetch, &query.filter)?;
        Ok(rrf_fuse(&[vector_hits, lexical_hits], query.k))
    }

    /// The read-path half of a model switch: the recorded model identity
    /// drives rebuilds during `reconcile`, but a search against a stale
    /// store must fail with a rebuild hint, never a backend-specific
    /// shape error. `Some(reason)` blocks the search:
    /// - recorded width != provider width (the provider's width is never
    ///   unknown — `EmbeddingProvider::dimensions` returns a `usize`);
    /// - recorded width unknown *while the store holds units*: an
    ///   unvalidated width can never be allowed through silently;
    ///   an empty store with no recorded width is fine (nothing to
    ///   compare, nothing to return).
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

    /// Whether the store holds any unit; a failed probe counts as holding
    /// (the search would surface the same failure moments later).
    fn store_has_units(&self) -> bool {
        self.store
            .unit_hashes()
            .map(|hashes| !hashes.is_empty())
            .unwrap_or(true)
    }
}

/// Unicode that renders as nothing (or as pure direction/formatting):
/// zero-width spaces and joiners, bidi marks and overrides, word joiners,
/// the BOM, interlinear annotations, and tag characters. Hidden payload
/// in these is the classic text-steganography channel for RAG poisoning —
/// invisible to a reader, fully visible to an embedder.
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

// The exact text embedded for a concept is per-passage now (see
// `chunk::chunk_concept`): invisible format characters are stripped there
// (`is_invisible_format`) and counted on each `Chunk` so the caller can
// warn. Stripping touches the indexed text only: bytes on disk stay exactly
// as written, so the integrity sidecar still verifies and the payload a
// poisoned book planted remains inspectable by `read`.

/// Builds the id callers use for [`ConceptId`] plumbing in tests.
#[cfg(test)]
fn concept_id(id: &str) -> crate::concept::ConceptId {
    id.parse().expect("test fixture ids are valid")
}

#[cfg(test)]
pub(crate) mod tests;
