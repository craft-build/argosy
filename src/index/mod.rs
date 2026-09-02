//! The semantic index: trait boundaries, embedding units, reconciliation,
//! and ranked search. The index is a derived, rebuildable artifact whose
//! only inputs are on-disk bundles and an [`EmbeddingProvider`]; custom
//! backends implement the traits, sqlite-vec + candle by default. One
//! unit per concept; search via [`Index::search`], lookup via resolve.

use std::collections::HashMap;

use crate::bundle::{Argosy, Namespace};
use crate::concept::Concept;
use crate::context::{ProjectContext, QualifiedConceptId};
use crate::error::{IndexSnafu, Result, UnknownArgosySnafu};
use crate::hash::sha256_hex;

#[cfg(feature = "default-index")]
pub mod candle;
#[cfg(feature = "default-index")]
pub mod sqlite;

/// Produces embedding vectors for texts. Batch-only: every call, even for a
/// single text, is a `&[String]` batch returning one vector per text in the
/// same order — this keeps the trait minimal and lets backends batch
/// efficiently.
pub trait EmbeddingProvider {
    /// The stable identity of the model — name and version/revision
    /// (e.g. `"candle/all-MiniLM-L6-v2@1"`). Compared against the
    /// store's recorded identity on every reconcile: vectors from different
    /// models are not comparable, so this must change with the weights.
    fn model_id(&self) -> &str;

    /// The vector dimensionality every [`EmbeddingProvider::embed`] call
    /// produces.
    fn dimensions(&self) -> usize;

    /// Embeds every text in `texts`, returning exactly one vector of
    /// [`EmbeddingProvider::dimensions`] floats per text, in order.
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
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
/// back to its source concept unambiguously. Multi-chunk concepts would
/// record one unit per chunk, distinguished by [`EmbeddingUnit::chunk_ordinal`];
/// in v1 there is always exactly one unit per concept (module docs).
#[derive(Debug, Clone)]
pub struct EmbeddingUnit {
    /// The source concept's qualified identity.
    pub concept: QualifiedConceptId,
    /// The unit's position within its source concept. Always `0`
    /// in v1's one-unit-per-concept chunking; kept from day one so a future
    /// multi-passage chunking strategy extends the data without changing it.
    pub chunk_ordinal: u32,
    /// The lowercase hex SHA-256 of the exact text that was embedded. Doubles
    /// as the staleness signal and the content-change detector
    ///: same text ⇒ same hash ⇒ no re-embed.
    pub text_hash: String,
    /// The unit's embedding vector.
    pub vector: Vec<f32>,
    /// The filterable facets of the source concept.
    pub meta: UnitMeta,
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
    /// Similarity score; hits are ordered by descending score.
    pub score: f32,
    /// The retrieved unit's facets, so callers can present/filter further
    /// without resolving the concept.
    pub meta: UnitMeta,
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
}

/// The report of one [`Index::reconcile`] run, for observability (printed by
/// the CLI's `index` subcommand).
#[derive(Debug, Clone, serde::Serialize)]
pub struct IndexReport {
    /// True iff the store was cleared and fully re-embedded.
    pub rebuilt: bool,
    /// Concepts embedded and upserted (new or content-changed).
    pub upserted: usize,
    /// Concepts removed from the store (deleted from disk).
    pub removed: usize,
    /// Concepts whose content hash was unchanged — each cost one hash and no
    /// embedding.
    pub unchanged: usize,
    /// The model identity the store now records.
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
/// batches are deterministic. Shared by [`Index::reconcile`] and
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
                    hash: sha256_hex(text.as_bytes()),
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
    /// (mirroring [`Index::reconcile`]'s guard).
    pub model_mismatch: bool,
}

/// A read-only, embed-free preview of what [`Index::reconcile`] would apply
/// (the CLI's `index status`): gathers the default namespace
/// selection from disk, hashes each concept, and diffs against
/// [`VectorStore::unit_hashes`]. Makes no embed calls and no writes.
pub fn staleness_report(
    context: &ProjectContext,
    store: &impl VectorStore,
    expected_model_id: &str,
) -> Result<StalenessReport> {
    let current = gather_concepts(context, &default_namespaces(), false)?;
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
    /// argosy only. Custom namespaces are not indexed by default; use
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
    /// active argosy — including `memory` of imports when listed. The
    /// selection is part of the index's identity: reconcile removes anything
    /// stored that the current selection no longer walks.
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
        gather_concepts(context, &self.namespaces, self.include_imported_memory)
    }

    /// Brings the store in line with the disk. An identity mismatch (or
    /// data with none recorded) clears the store **before** re-embedding,
    /// so it never holds two models' vectors. Otherwise a diff: only new or
    /// changed concepts are embedded, gone ones removed — reconcile scales
    /// with what changed. Returns an [`IndexReport`]; fully rebuildable.
    pub fn reconcile(&mut self, context: &ProjectContext) -> Result<IndexReport> {
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
        // current identity. The rebuild branch does not rely on
        // `clear()` having dropped the identity: a store that kept it would
        // otherwise reconcile against a stale identity on every run and
        // clear + re-embed the whole corpus each time.
        if report.rebuilt || self.store.model_id().is_none() {
            self.store.set_model_id(&model)?;
        }

        Ok(report)
    }

    /// Ranked semantic search: embeds [`Query::text`] and returns the
    /// store's `k` most similar hits under [`Query::filter`]; for direct
    /// lookup use [`ProjectContext::resolve`]. Names in `filter.argosies`
    /// must be active in `context` — else
    /// [`crate::error::Error::UnknownArgosy`], never a silent empty.
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
pub(crate) mod tests;
