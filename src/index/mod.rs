//! The semantic index: trait boundaries, embedding units, reconciliation,
//! and ranked search (spec §7 `IDX-1`–`IDX-13`, §10 `QRY-1`–`QRY-3`/
//! `QRY-6`/`QRY-7`, `DIST-6`, `NFR-4`).
//!
//! **The index is a derived, rebuildable artifact** (spec §3.1): its only
//! inputs are the on-disk bundle contents of a [`ProjectContext`] and an
//! [`EmbeddingProvider`]. Nothing here persists anything itself or requires a
//! specific backend — the crate ships the two traits [`EmbeddingProvider`]
//! and [`VectorStore`], plus an [`Index`] engine written against them, and a
//! consumer (e.g. Craft) may supply its own embedding stack by implementing
//! the two traits and ignoring the default backend entirely. The default
//! backend (sqlite-vec + fastembed) fills these traits behind the
//! `default-index` Cargo feature: [`sqlite::SqliteVecStore`] and
//! [`fastembed::FastembedProvider`]. With the feature off this module is
//! dependency-free and the traits stand alone.
//!
//! **Chunking decision (locked per doc 06 §1).** One embedding unit per
//! concept; the unit text is the concept's `description` (when present) plus
//! its body. Concept-scale retrieval matches the spec's retrieval model
//! (results are concepts, `IDX-3`), keeps traceability trivial, and avoids a
//! premature chunking algorithm (spec §15 lists canonical chunking as future
//! work). Multi-chunk embedding remains possible behind
//! [`EmbeddingUnit::chunk_ordinal`], which exists from day one (`IDX-4`).
//!
//! **Retrieval modes.** Ranked semantic retrieval is [`Index::search`]
//! (`QRY-1`); direct lookup of a known concept is
//! [`ProjectContext::resolve`]/[`ProjectContext::read_uri`] (`QRY-4`) — the
//! two complement each other and share [`QualifiedConceptId`] as the identity
//! currency, so any hit can be resolved to the full concept.

mod sha256;

#[cfg(feature = "default-index")]
pub mod fastembed;
#[cfg(feature = "default-index")]
pub mod sqlite;

use std::collections::HashMap;

use crate::bundle::{Argosy, Namespace};
use crate::concept::Concept;
use crate::context::{ProjectContext, QualifiedConceptId};
use crate::error::{IndexSnafu, Result, UnknownArgosySnafu};

/// Produces embedding vectors for texts. Batch-only: every call, even for a
/// single text, is a `&[String]` batch returning one vector per text in the
/// same order — this keeps the trait minimal and lets backends batch
/// efficiently.
pub trait EmbeddingProvider {
    /// The stable identity of the model, including both name and
    /// version/revision (e.g. `"fastembed/all-MiniLM-L6-v2@4"`). Recorded in
    /// the index (`IDX-5`) and compared against the store's recorded identity
    /// on every reconcile (`IDX-12`): vectors from different models are not
    /// comparable (`IDX-6`), so identity must change whenever the weights or
    /// weights format change.
    fn model_id(&self) -> &str;

    /// The vector dimensionality every [`EmbeddingProvider::embed`] call
    /// produces.
    fn dimensions(&self) -> usize;

    /// Embeds every text in `texts`, returning exactly one vector of
    /// [`EmbeddingProvider::dimensions`] floats per text, in order.
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
}

/// The structured metadata a query can filter on (`IDX-9`, `QRY-3`, `STG-4`):
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
    /// The frontmatter `language` facet (styleguide rules, `STG-4`).
    pub language: Option<String>,
    /// The frontmatter `category` facet (styleguide rules, `STG-4`).
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

/// One embedded unit of a concept (`IDX-3`): enough to trace any retrieval
/// back to its source concept unambiguously. Multi-chunk concepts would
/// record one unit per chunk, distinguished by [`EmbeddingUnit::chunk_ordinal`];
/// in v1 there is always exactly one unit per concept (module docs).
#[derive(Debug, Clone)]
pub struct EmbeddingUnit {
    /// The source concept's qualified identity (`IDX-3` traceability).
    pub concept: QualifiedConceptId,
    /// The unit's position within its source concept (`IDX-4`). Always `0`
    /// in v1's one-unit-per-concept chunking; kept from day one so a future
    /// multi-passage chunking strategy extends the data without changing it.
    pub chunk_ordinal: u32,
    /// The lowercase hex SHA-256 of the exact text that was embedded. Doubles
    /// as the staleness signal (`IDX-11`) and the content-change detector
    /// (`DIST-6`): same text ⇒ same hash ⇒ no re-embed (`NFR-4`).
    pub text_hash: String,
    /// The unit's embedding vector.
    pub vector: Vec<f32>,
    /// The filterable facets of the source concept (`IDX-9`).
    pub meta: UnitMeta,
}

/// The structured constraints a search composes with its query text
/// (`IDX-8`/`IDX-9`, `QRY-2`/`QRY-3`). Every field is optional; `None` means
/// unconstrained.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    /// Only return units in these namespaces (`IDX-8`).
    pub namespaces: Option<Vec<Namespace>>,
    /// Only return units from these argosies (manifest names, `QRY-2`).
    /// [`Index::search`] validates every name against the context's active
    /// set before reaching the store; individual stores may assume validity.
    pub argosies: Option<Vec<String>>,
    /// Only return units whose `type` is one of these (`IDX-9`, `QRY-3`).
    pub concept_types: Option<Vec<String>>,
    /// Only return units carrying at least one of these tags (`IDX-9`).
    pub tags: Option<Vec<String>>,
    /// Only return units with this exact `language` facet (`STG-4`).
    pub language: Option<String>,
    /// Only return units with this exact `category` facet (`STG-4`).
    pub category: Option<String>,
}

/// One ranked search result (`IDX-7`).
#[derive(Debug, Clone, serde::Serialize)]
pub struct SearchHit {
    /// The retrieved concept's qualified identity — origin argosy visible
    /// (`QRY-6`).
    pub concept: QualifiedConceptId,
    /// Similarity score; hits are ordered by descending score (`IDX-7`).
    pub score: f32,
    /// The retrieved unit's facets, so callers can present/filter further
    /// without resolving the concept.
    pub meta: UnitMeta,
}

/// One semantic query (`QRY-1`).
#[derive(Debug, Clone)]
pub struct Query {
    /// The natural-language query text.
    pub text: String,
    /// Maximum number of hits to return.
    pub k: usize,
    /// Structured narrowing composed with the semantic ranking (`QRY-3`).
    pub filter: Filter,
}

impl Query {
    /// An unscoped query (`QRY-6`): searches every active argosy and
    /// namespace the store holds.
    pub fn unscoped(text: impl Into<String>, k: usize) -> Self {
        Self {
            text: text.into(),
            k,
            filter: Filter::default(),
        }
    }
}

/// Stores and retrieves embedding units. Implementations own the ranking
/// contract: [`VectorStore::search`] returns hits ordered by descending
/// similarity (`IDX-7`). `argosy` ships a sqlite-vec implementation behind
/// the `default-index` feature; any store honoring this contract composes
/// with [`Index`].
pub trait VectorStore {
    /// The model identity recorded for the vectors the store currently
    /// holds (`IDX-5`), or `None` before the first reconcile and after
    /// [`VectorStore::clear`].
    fn model_id(&self) -> Option<&str>;

    /// Records the model identity of the store's current contents (set by
    /// [`Index::reconcile`] after a (re)build, `IDX-5`).
    fn set_model_id(&mut self, id: &str) -> Result<()>;

    /// Inserts or replaces units keyed by their source concept (`IDX-10`
    /// incremental update: re-upserting a concept must not duplicate it).
    fn upsert(&mut self, units: &[EmbeddingUnit]) -> Result<()>;

    /// Drops every unit of one concept (`IDX-10` incremental deletion).
    fn remove_concept(&mut self, concept: &QualifiedConceptId) -> Result<()>;

    /// The stored `text_hash` of every concept — the diff input
    /// [`Index::reconcile`] compares against freshly hashed content
    /// (`IDX-11`), so unchanged concepts never reach the embedder (`NFR-4`).
    fn unit_hashes(&self) -> Result<HashMap<QualifiedConceptId, String>>;

    /// Drops every unit — the full-rebuild path (`IDX-12`). Dropping the
    /// recorded model identity too is recommended but not required:
    /// [`Index::reconcile`] re-records the identity explicitly after every
    /// rebuild rather than relying on this side effect.
    fn clear(&mut self) -> Result<()>;

    /// The `k` units most similar to `vector` under `filter`, ordered by
    /// descending similarity (`IDX-7`–`IDX-9`).
    fn search(&self, vector: &[f32], k: usize, filter: &Filter) -> Result<Vec<SearchHit>>;
}

/// The report of one [`Index::reconcile`] run, for observability (printed by
/// the CLI's `index` subcommand).
#[derive(Debug, Clone, serde::Serialize)]
pub struct IndexReport {
    /// True iff the store was cleared and fully re-embedded (`IDX-12`).
    pub rebuilt: bool,
    /// Concepts embedded and upserted (new or content-changed).
    pub upserted: usize,
    /// Concepts removed from the store (deleted from disk).
    pub removed: usize,
    /// Concepts whose content hash was unchanged — each cost one hash and no
    /// embedding (`NFR-4`).
    pub unchanged: usize,
    /// The model identity the store now records (`IDX-5`).
    pub model_id: String,
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
/// batches are deterministic. `memory` of *imported* argosies is skipped
/// unless `include_imported_memory` (the default-local-memory rule from
/// [`Index::new`]). Shared by [`Index::reconcile`] and
/// [`staleness_report`], which must never drift apart.
fn gather_concepts(
    context: &ProjectContext,
    namespaces: &[Namespace],
    include_imported_memory: bool,
) -> Result<Vec<GatheredConcept>> {
    let mut out = Vec::new();
    let mut visit = |name: &str, argosy: &Argosy, is_local: bool| -> Result<()> {
        for namespace in namespaces {
            if !is_local && !include_imported_memory && *namespace == Namespace::Memory {
                continue;
            }
            for (id, concept) in argosy.concepts(namespace)? {
                let text = embed_text(&concept);
                out.push(GatheredConcept {
                    qid: QualifiedConceptId {
                        argosy: name.to_string(),
                        namespace: namespace.clone(),
                        id,
                    },
                    hash: sha256::sha256_hex(text.as_bytes()),
                    text,
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
    /// (`IDX-12`, mirroring [`Index::reconcile`]'s guard).
    pub model_mismatch: bool,
}

/// A read-only, embed-free preview of what [`Index::reconcile`] would apply
/// (the CLI's `index status`, doc 09): gathers the default namespace
/// selection from disk, hashes each concept, and diffs against
/// [`VectorStore::unit_hashes`]. Makes no embed calls and no writes.
pub fn staleness_report(
    context: &ProjectContext,
    store: &impl VectorStore,
    expected_model_id: &str,
) -> Result<StalenessReport> {
    let current = gather_concepts(context, &default_namespaces(), false)?;
    let stored = store.unit_hashes()?;

    // Mirrors reconcile's identity check exactly (IDX-12): a store holding
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
    text: String,
    hash: String,
    meta: UnitMeta,
}

/// The reconcile/query engine, written against the [`EmbeddingProvider`] and
/// [`VectorStore`] trait boundaries only. Owns the provider and the store.
pub struct Index<P: EmbeddingProvider, S: VectorStore> {
    provider: P,
    store: S,
    namespaces: Vec<Namespace>,
    include_imported_memory: bool,
}

impl<P: EmbeddingProvider, S: VectorStore> Index<P, S> {
    /// An index over the default namespace set: `document`, `skill`, and
    /// `styleguide` of every active argosy, plus `memory` of the *local*
    /// argosy only (memory search is a §10.2 recommendation and unscoped
    /// `QRY-6` queries include it when local; imported memory stays out).
    ///
    /// Custom namespaces are not indexed by default; use
    /// [`Index::with_namespaces`].
    pub fn new(provider: P, store: S) -> Self {
        Self {
            provider,
            store,
            namespaces: default_namespaces(),
            include_imported_memory: false,
        }
    }

    /// An index over an explicit namespace set, honored verbatim for every
    /// active argosy — including `memory` of imported argosies when listed.
    ///
    /// The selection is part of the index's identity: reconcile removes
    /// anything stored that the current selection no longer walks, so
    /// narrowing the set between reconciles deletes units wholesale.
    pub fn with_namespaces(provider: P, store: S, namespaces: Vec<Namespace>) -> Self {
        Self {
            provider,
            store,
            namespaces,
            include_imported_memory: true,
        }
    }

    /// The embedding provider.
    pub fn provider(&self) -> &P {
        &self.provider
    }

    /// Replaces the provider (e.g. after a model upgrade). The next
    /// [`Index::reconcile`] detects the identity change (`IDX-12`) and
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
        gather_concepts(context, &self.namespaces, self.include_imported_memory)
    }

    /// Brings the store in line with the disk: the incremental maintenance
    /// loop (`IDX-10`, `NFR-4`, lifecycle §11 steps 3 and 6).
    ///
    /// 1. **Model check (`IDX-12`).** If the store's recorded identity
    ///    differs from the provider's, or it holds data with no recorded
    ///    identity, the whole index is stale: the store is cleared **before**
    ///    any new embedding runs and fully re-embedded. This
    ///    clear-before-re-embed ordering is the enforcement point of
    ///    `IDX-13` — by construction the store can never hold two models'
    ///    vectors, so a query can never mix them silently.
    /// 2. **Otherwise, diff.** Each concept's fresh content hash is compared
    ///    against [`VectorStore::unit_hashes`]; only new or changed concepts
    ///    reach the embedder, stored-but-gone concepts are removed
    ///    (`IDX-10`/`IDX-11`). Unchanged concepts cost one hash each —
    ///    reconcile scales with what changed, not the bundle size (`NFR-4`).
    /// 3. The store's model identity is (re)recorded (`IDX-5`) and an
    ///    [`IndexReport`] returned.
    ///
    /// `IDX-1`/`IDX-2` by construction: reconcile's only inputs are the
    /// bundles' contents and the provider, so a markdown-only argosy is fully
    /// usable after one reconcile and the index can always be rebuilt from
    /// scratch.
    pub fn reconcile(&mut self, context: &ProjectContext) -> Result<IndexReport> {
        let current = self.gather(context)?;
        let stored = self.store.unit_hashes()?;
        let model = self.provider.model_id().to_string();

        let identity_mismatch = match self.store.model_id() {
            Some(recorded) => recorded != model,
            // Data with no recorded identity cannot be trusted to match this
            // provider either — treat it as a mismatch (`IDX-12`).
            None => !stored.is_empty(),
        };

        let mut report = IndexReport {
            rebuilt: identity_mismatch,
            upserted: 0,
            removed: 0,
            unchanged: 0,
            model_id: model.clone(),
        };

        let prior: HashMap<QualifiedConceptId, String> = if identity_mismatch {
            self.store.clear()?;
            HashMap::new()
        } else {
            stored
        };

        // Deletions: stored concepts no longer on disk under any walked
        // namespace (IDX-10). The lookup set keeps this O(stored + current)
        // rather than O(stored * current).
        let on_disk: std::collections::HashSet<&QualifiedConceptId> =
            current.iter().map(|c| &c.qid).collect();
        for qid in prior.keys() {
            if !on_disk.contains(qid) {
                self.store.remove_concept(qid)?;
                report.removed += 1;
            }
        }

        // New or content-changed concepts (IDX-11); everything else is one
        // hash and done (NFR-4).
        let mut to_embed = Vec::new();
        for gathered in &current {
            match prior.get(&gathered.qid) {
                Some(hash) if *hash == gathered.hash => report.unchanged += 1,
                _ => to_embed.push(gathered),
            }
        }

        if !to_embed.is_empty() {
            let texts: Vec<String> = to_embed.iter().map(|g| g.text.clone()).collect();
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
            let units: Vec<EmbeddingUnit> = to_embed
                .iter()
                .zip(vectors)
                .map(|(g, vector)| EmbeddingUnit {
                    concept: g.qid.clone(),
                    chunk_ordinal: 0,
                    text_hash: g.hash.clone(),
                    vector,
                    meta: g.meta.clone(),
                })
                .collect();
            report.upserted = units.len();
            self.store.upsert(&units)?;
        }

        // First reconcile (no identity yet) and every rebuild record the
        // current identity (IDX-5). The rebuild branch does not rely on
        // `clear()` having dropped the identity: a store that kept it would
        // otherwise reconcile against a stale identity on every run and
        // clear + re-embed the whole corpus each time.
        if report.rebuilt || self.store.model_id().is_none() {
            self.store.set_model_id(&model)?;
        }

        Ok(report)
    }

    /// Ranked semantic search (`QRY-1`): embeds [`Query::text`] and returns
    /// the store's `k` most similar hits under [`Query::filter`]. For direct
    /// lookup of a known concept, use [`ProjectContext::resolve`] (`QRY-4`) —
    /// the two modes share [`QualifiedConceptId`], so a hit here resolves
    /// there.
    ///
    /// `QRY-2`/`QRY-6`: every name in `filter.argosies` must be an active
    /// argosy of `context` — naming an inactive argosy is
    /// [`crate::error::Error::UnknownArgosy`], not an empty result (a silent
    /// empty would hide a configuration mistake). An unscoped query searches
    /// everything the store holds across all active argosies.
    ///
    /// `QRY-7`: hits are ordered by score alone. No precedence information
    /// from `context` is threaded into the store, so local-first boosting is
    /// impossible by construction.
    pub fn search(&self, context: &ProjectContext, query: &Query) -> Result<Vec<SearchHit>> {
        if let Some(argosies) = &query.filter.argosies {
            for name in argosies {
                if context.argosy_named(name).is_none() {
                    return UnknownArgosySnafu { name: name.clone() }.fail();
                }
            }
        }
        let texts = std::slice::from_ref(&query.text);
        let mut vectors = self.provider.embed(texts)?;
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
        self.store.search(&vector, query.k, &query.filter)
    }
}

/// The exact text that gets embedded for a concept (module docs, chunking
/// decision): `description` plus body when a description exists, body alone
/// otherwise.
fn embed_text(concept: &Concept) -> String {
    match concept.description() {
        Some(description) if !description.trim().is_empty() => {
            format!("{}\n\n{}", description, concept.body())
        }
        _ => concept.body().to_string(),
    }
}

/// Builds the id callers use for [`ConceptId`] plumbing in tests.
#[cfg(test)]
fn concept_id(id: &str) -> crate::concept::ConceptId {
    id.parse().expect("test fixture ids are valid")
}

#[cfg(test)]
pub(crate) mod tests {
    use std::cell::Cell;
    use std::collections::HashMap;
    use std::fs;
    use std::path::Path;

    use tempfile::TempDir;

    use super::*;
    use crate::concept::Concept;
    use crate::error::Error;

    // --- Test doubles: MockEmbedder + MemStore prove trait sufficiency with
    // no ONNX and no SQLite (doc 06 §2.6). ---

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

        /// A provider with a different identity, to simulate model flips
        /// (`IDX-12`).
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
    /// `Filter` field. Also records bookkeeping (`clears`, `removals`) so
    /// tests can assert reconcile's calls.
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
            // Descending similarity (IDX-7). Ties keep whatever order the
            // filter walk produced — never an argosy-precedence order,
            // because the store never sees precedence (QRY-7).
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
    const MEMORY_GOTCHAS: &str = "---\ntype: Note\ndescription: Build gotchas.\ntags: [build]\n---\nCargo needs a lockfile.\n";
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

    // --- staleness_report: the read-only preview must mirror reconcile ---

    #[test]
    fn staleness_after_reconcile_is_all_unchanged() {
        let (_local, _imported, ctx) = fixture();
        let mut index = fresh_index();
        index.reconcile(&ctx).unwrap();

        let report = staleness_report(&ctx, index.store(), "mock-embedder@1").unwrap();

        assert_eq!(report.added, 0);
        assert_eq!(report.changed, 0);
        assert_eq!(report.removed, 0);
        assert_eq!(report.unchanged, 5);
        assert!(!report.model_mismatch);
    }

    #[test]
    fn staleness_preview_matches_the_diff_reconcile_applies() {
        let (local, _imported, ctx) = fixture();
        let mut index = fresh_index();
        index.reconcile(&ctx).unwrap();

        // Edit one concept, delete one, add one.
        write_file(local.path(), "memory/gotchas.md", "Edited build notes.\n");
        fs::remove_file(local.path().join("skill/deploy.md")).unwrap();
        write_file(
            local.path(),
            "document/new-note.md",
            "---\ntype: Note\n---\nA new note.\n",
        );

        let report = staleness_report(&ctx, index.store(), "mock-embedder@1").unwrap();

        assert_eq!(report.added, 1, "the new document counts as added");
        assert_eq!(report.changed, 1, "the edited memory counts as changed");
        assert_eq!(report.removed, 1, "the deleted skill counts as removed");
        assert_eq!(report.unchanged, 3);
        assert!(!report.model_mismatch);
    }

    #[test]
    fn staleness_agrees_with_reconcile_on_identity_states() {
        let (_local, _imported, ctx) = fixture();
        let mut index = fresh_index();
        index.reconcile(&ctx).unwrap();

        // A recorded identity differing from the expected model ⇒ mismatch
        // (reconcile would rebuild, IDX-12).
        let report = staleness_report(&ctx, index.store(), "other-model@2").unwrap();
        assert!(report.model_mismatch);

        // Data present with NO recorded identity: reconcile's `None =>
        // !stored.is_empty()` guard rebuilds, so the preview must not claim
        // "up to date".
        index.store_mut().model_id = None;
        let report = staleness_report(&ctx, index.store(), "mock-embedder@1").unwrap();
        assert!(
            report.model_mismatch,
            "data without a recorded identity is a rebuild, never \"up to date\""
        );

        // An empty store with no recorded identity announces nothing; the
        // added counts carry the whole story.
        index.store_mut().clear().unwrap();
        let report = staleness_report(&ctx, index.store(), "mock-embedder@1").unwrap();
        assert!(!report.model_mismatch);
        assert_eq!(report.added, 5);
    }

    // --- First reconcile, IDX-1/IDX-2 ---

    #[test]
    fn first_reconcile_embeds_every_default_namespace_concept_and_records_model() {
        let (_local, _imported, ctx) = fixture();
        let mut index = fresh_index();

        let report = index.reconcile(&ctx).unwrap();

        // local: arch + deploy + gotchas + case; vendor: locking (its
        // memory/ is skipped by the default walk).
        assert_eq!(report.upserted, 5);
        assert_eq!(report.removed, 0);
        assert_eq!(report.unchanged, 0);
        assert!(!report.rebuilt);
        assert_eq!(report.model_id, "mock-embedder@1");

        let store = index.store();
        assert_eq!(store.model_id(), Some("mock-embedder@1"));
        let hashes = store.unit_hashes().unwrap();
        assert_eq!(hashes.len(), 5);
        assert!(
            hashes
                .keys()
                .any(|q| q.argosy == "local" && q.namespace == Namespace::Memory)
        );
        assert!(
            !hashes
                .keys()
                .any(|q| q.argosy == "vendor" && q.namespace == Namespace::Memory)
        );
    }

    // --- Unchanged reconcile, NFR-4/IDX-10 ---

    #[test]
    fn second_reconcile_with_no_changes_costs_only_hashing() {
        let (_local, _imported, ctx) = fixture();
        let mut index = fresh_index();
        index.reconcile(&ctx).unwrap();
        let embed_calls_before = index.provider().embed_calls();

        let report = index.reconcile(&ctx).unwrap();

        assert_eq!(report.upserted, 0);
        assert_eq!(report.removed, 0);
        assert_eq!(report.unchanged, 5, "nothing changed ⇒ nothing re-embedded");
        assert!(!report.rebuilt);
        assert_eq!(
            index.provider().embed_calls(),
            embed_calls_before,
            "NFR-4: an unchanged concept costs one hash, never an embed"
        );
    }

    // --- Incremental edit + delete, IDX-10/IDX-11 ---

    #[test]
    fn editing_one_concept_upserts_exactly_it_and_deleting_removes_it() {
        let (local, _imported, ctx) = fixture();
        let mut index = fresh_index();
        index.reconcile(&ctx).unwrap();

        // Edit the memory note in place through the write surface.
        let edited = Concept::from_str(
            "---\ntype: Note\ndescription: Build gotchas.\ntags: [build]\n---\nCargo needs a workspace.\n",
        )
        .unwrap();
        ctx.local()
            .write_concept(Namespace::Memory, &concept_id("memory/gotchas"), &edited)
            .unwrap();
        let old_hashes = index.store().unit_hashes().unwrap();

        let report = index.reconcile(&ctx).unwrap();
        assert_eq!(
            report.upserted, 1,
            "IDX-11: only the changed concept is stale"
        );
        assert_eq!(report.unchanged, 4);
        assert_eq!(report.removed, 0);
        let qid = QualifiedConceptId {
            argosy: "local".to_string(),
            namespace: Namespace::Memory,
            id: concept_id("memory/gotchas"),
        };
        let new_hashes = index.store().unit_hashes().unwrap();
        assert_ne!(old_hashes[&qid], new_hashes[&qid]);

        // Delete it: the next reconcile calls remove_concept, and search can
        // no longer return it.
        ctx.local()
            .delete_concept(Namespace::Memory, &concept_id("memory/gotchas"))
            .unwrap();
        let report = index.reconcile(&ctx).unwrap();
        assert_eq!(report.removed, 1, "IDX-10: deletion is incremental");
        assert_eq!(report.upserted, 0);
        assert_eq!(index.store().removals, vec![qid.clone()]);
        let hits = index
            .search(&ctx, &Query::unscoped("Cargo lockfile build gotchas", 10))
            .unwrap();
        assert!(
            hits.iter().all(|h| h.concept != qid),
            "a deleted concept must disappear from search"
        );

        let _ = local;
    }

    // --- Model flip, IDX-12/IDX-13 ---

    #[test]
    fn flipping_the_model_id_clears_and_fully_rebuilds() {
        let (_local, _imported, ctx) = fixture();
        let mut index = fresh_index();
        index.reconcile(&ctx).unwrap();
        let before = index.store().unit_hashes().unwrap();

        index.set_provider(MockEmbedder::with_model_id("mock-embedder@2"));
        let report = index.reconcile(&ctx).unwrap();

        assert!(
            report.rebuilt,
            "IDX-12: identity mismatch stales the whole index"
        );
        assert_eq!(report.upserted, 5);
        assert_eq!(report.unchanged, 0);
        assert_eq!(report.removed, 0);
        assert_eq!(report.model_id, "mock-embedder@2");
        assert_eq!(index.store().model_id(), Some("mock-embedder@2"));
        assert_eq!(index.store().clears, 1, "clear happened before re-embed");

        // The same concepts are re-stored under the new identity and are
        // searchable — the cleared old vectors are gone entirely (IDX-13:
        // interleaving old and new models is impossible by construction).
        assert!(index.store().unit_hashes().unwrap() == before);
        let hits = index
            .search(&ctx, &Query::unscoped("architecture", 10))
            .unwrap();
        assert!(!hits.is_empty());
    }

    #[test]
    fn rebuild_re_records_identity_even_if_clear_keeps_the_old_one() {
        // Regression for the mismatch guard previously relying on `clear()`
        // dropping the identity: a store that keeps it must still end the
        // reconcile recording the new model, or every later reconcile would
        // see a mismatch and clear + re-embed the whole corpus again.
        /// A `MemStore` whose `clear()` drops units but keeps `model_id`.
        struct StickyClear(MemStore);

        impl VectorStore for StickyClear {
            fn model_id(&self) -> Option<&str> {
                self.0.model_id()
            }
            fn set_model_id(&mut self, id: &str) -> Result<()> {
                self.0.set_model_id(id)
            }
            fn upsert(&mut self, units: &[EmbeddingUnit]) -> Result<()> {
                self.0.upsert(units)
            }
            fn remove_concept(&mut self, concept: &QualifiedConceptId) -> Result<()> {
                self.0.remove_concept(concept)
            }
            fn unit_hashes(&self) -> Result<HashMap<QualifiedConceptId, String>> {
                self.0.unit_hashes()
            }
            fn clear(&mut self) -> Result<()> {
                let keep = self.0.model_id.clone();
                self.0.units.clear();
                self.0.model_id = keep;
                Ok(())
            }
            fn search(&self, vector: &[f32], k: usize, filter: &Filter) -> Result<Vec<SearchHit>> {
                self.0.search(vector, k, filter)
            }
        }

        let (_local, _imported, ctx) = fixture();
        let mut index = Index::new(MockEmbedder::new(), StickyClear(MemStore::new()));
        index.reconcile(&ctx).unwrap();

        index.set_provider(MockEmbedder::with_model_id("mock-embedder@2"));
        let report = index.reconcile(&ctx).unwrap();
        assert!(report.rebuilt);
        assert_eq!(index.store().model_id(), Some("mock-embedder@2"));

        // The next reconcile is incremental, not another full rebuild.
        let report = index.reconcile(&ctx).unwrap();
        assert!(!report.rebuilt);
        assert_eq!(report.upserted, 0);
        assert_eq!(report.unchanged, 5);
    }

    // --- Namespace + facet filters, IDX-8/IDX-9, QRY-2/QRY-3 ---

    #[test]
    fn namespace_filter_scopes_search_to_the_selected_namespaces() {
        let (_local, _imported, ctx) = fixture();
        let mut index = fresh_index();
        index.reconcile(&ctx).unwrap();

        let mut query = Query::unscoped("naming case style", 10);
        query.filter.namespaces = Some(vec![Namespace::Styleguide]);
        let hits = index.search(&ctx, &query).unwrap();

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].concept.namespace, Namespace::Styleguide);
        assert_eq!(hits[0].concept.argosy, "local");
        assert_eq!(
            hits[0].concept.id,
            concept_id("styleguide/rust/naming/case")
        );

        // Skills only: the styleguide hit is gone.
        query.filter.namespaces = Some(vec![Namespace::Skill]);
        let hits = index.search(&ctx, &query).unwrap();
        assert!(hits.iter().all(|h| h.concept.namespace == Namespace::Skill));
    }

    #[test]
    fn facet_filters_narrow_individually_and_compose_with_semantics() {
        let (_local, _imported, ctx) = fixture();
        let mut index = fresh_index();
        index.reconcile(&ctx).unwrap();

        let mut query = Query::unscoped("service", 10);

        // language + category (STG-4 facets, IDX-9).
        query.filter.language = Some("rust".to_string());
        let hits = index.search(&ctx, &query).unwrap();
        assert_eq!(hits.len(), 1, "only the styleguide rule declares language");
        assert_eq!(hits[0].meta.language.as_deref(), Some("rust"));
        query.filter.category = Some("naming".to_string());
        let hits = index.search(&ctx, &query).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].meta.category.as_deref(), Some("naming"));
        assert_eq!(
            hits[0].meta.concept_type.as_deref(),
            Some("Styleguide Rule")
        );
        query.filter.language = None;
        query.filter.category = None;

        // tags (any-of): only the imported locking note is tagged `database`.
        query.filter.tags = Some(vec!["database".to_string()]);
        let hits = index.search(&ctx, &query).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].concept.argosy, "vendor");
        assert!(hits[0].meta.tags.contains(&"database".to_string()));
        query.filter.tags = None;

        // concept_types: only the deploy entry point is a Skill.
        query.filter.concept_types = Some(vec!["Skill".to_string()]);
        let hits = index.search(&ctx, &query).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].concept.id, concept_id("skill/deploy"));
        query.filter.concept_types = None;

        // Semantic + structured in one call (QRY-3): the database-tagged
        // locking note tops a `database locking` semantic query, and the
        // namespace constraint still holds.
        let mut query = Query::unscoped("database locking retries", 10);
        query.filter.namespaces = Some(vec![Namespace::Document]);
        let hits = index.search(&ctx, &query).unwrap();
        assert!(!hits.is_empty());
        assert_eq!(hits[0].concept.argosy, "vendor");
        assert!(
            hits.iter()
                .all(|h| h.concept.namespace == Namespace::Document)
        );
        assert!(
            hits[0].score > hits[1].score,
            "ranking follows similarity to the query"
        );
    }

    // --- Unscoped search across argosys, QRY-6/QRY-7 ---

    #[test]
    fn unscoped_search_spans_argosys_with_score_only_ranking_and_no_precedence_boost() {
        // Identical text in the local and an imported argosy: their scores
        // must tie exactly — any local-first reordering would require
        // precedence data the search path never receives (QRY-7).
        const SAME: &str = "---\ntype: Note\ndescription: Shared body.\n---\nalpha beta gamma.\n";
        let local = make_argosy("local", &[("document/shared.md", SAME)]);
        let imported = make_argosy("vendor", &[("document/shared.md", SAME)]);
        let ctx = ProjectContext::open(local.path(), [imported.path().to_path_buf()]).unwrap();
        let mut index = fresh_index();
        index.reconcile(&ctx).unwrap();

        let hits = index
            .search(&ctx, &Query::unscoped("alpha beta gamma", 10))
            .unwrap();
        assert_eq!(hits.len(), 2, "QRY-6: unscoped search spans both argosies");
        let origins: Vec<&str> = hits.iter().map(|h| h.concept.argosy.as_str()).collect();
        assert!(origins.contains(&"local") && origins.contains(&"vendor"));
        assert_eq!(
            hits[0].score, hits[1].score,
            "identical text ⇒ identical score; no precedence reweighting"
        );

        // Scoping to a single argosy by name narrows to it (QRY-2).
        let mut query = Query::unscoped("alpha beta gamma", 10);
        query.filter.argosies = Some(vec!["vendor".to_string()]);
        let hits = index.search(&ctx, &query).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].concept.argosy, "vendor");
    }

    #[test]
    fn scoping_search_to_an_inactive_argosy_is_an_error_not_an_empty_result() {
        let (_local, _imported, ctx) = fixture();
        let mut index = fresh_index();
        index.reconcile(&ctx).unwrap();

        let mut query = Query::unscoped("anything", 10);
        query.filter.argosies = Some(vec!["ghost".to_string()]);
        let err = index.search(&ctx, &query).unwrap_err();
        assert!(
            matches!(err, Error::UnknownArgosy { ref name } if name == "ghost"),
            "inactive argosy names must surface a config error, got {err}"
        );
    }

    // --- Local memory search, imported memory excluded (default walk) ---

    #[test]
    fn local_memory_is_searchable_but_imported_memory_stays_out_of_the_default_walk() {
        let (_local, _imported, ctx) = fixture();
        let mut index = fresh_index();
        index.reconcile(&ctx).unwrap();

        // The two memory notes share identical text, so with no walk
        // difference both would appear.
        let hits = index
            .search(&ctx, &Query::unscoped("Cargo lockfile build gotchas", 10))
            .unwrap();
        let memory_hits: Vec<_> = hits
            .iter()
            .filter(|h| h.concept.namespace == Namespace::Memory)
            .collect();
        assert_eq!(memory_hits.len(), 1, "exactly one memory note is indexed");
        assert_eq!(memory_hits[0].concept.argosy, "local");

        // An imported argosy without memory/ contributes none either.
        let local2 = make_argosy("local2", &[]);
        let imported2 = make_argosy("plain", &[("document/x.md", DOC_ARCH)]);
        let ctx2 = ProjectContext::open(local2.path(), [imported2.path().to_path_buf()]).unwrap();
        let mut index2 = fresh_index();
        index2.reconcile(&ctx2).unwrap();
        assert!(
            index2
                .store()
                .unit_hashes()
                .unwrap()
                .keys()
                .all(|q| q.namespace != Namespace::Memory)
        );
    }

    // --- Explicit namespace selection (custom walk + honored imported memory) ---

    #[test]
    fn with_namespaces_honors_the_selection_verbatim_for_every_argosy() {
        let local = make_argosy("local", &[("document/arch.md", DOC_ARCH)]);
        let imported = make_argosy("vendor", &[("memory/vendor-notes.md", MEMORY_GOTCHAS)]);
        let ctx = ProjectContext::open(local.path(), [imported.path().to_path_buf()]).unwrap();

        // Memory only: local arch is excluded; vendor memory is included —
        // explicit selection overrides the default local-only memory rule.
        let mut index = Index::with_namespaces(
            MockEmbedder::new(),
            MemStore::new(),
            vec![Namespace::Memory],
        );
        let report = index.reconcile(&ctx).unwrap();
        assert_eq!(report.upserted, 1);
        let hashes = index.store().unit_hashes().unwrap();
        assert_eq!(hashes.len(), 1);
        let qid = hashes.keys().next().unwrap();
        assert_eq!(qid.argosy, "vendor");
        assert_eq!(qid.namespace, Namespace::Memory);
    }
}
