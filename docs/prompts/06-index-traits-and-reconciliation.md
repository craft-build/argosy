# 06 — Index Traits, Embedding Units, Staleness, and Reconciliation

| | |
|---|---|
| Depends on | 05 |
| Creates | `src/index/mod.rs`; extends `src/lib.rs`, `src/error.rs`, `Cargo.toml` (feature `default-index`, no new deps yet) |
| Spec sections | §3.1 (`IDX-1`/`IDX-2`), §7.2 (`IDX-3`/`IDX-4`), §7.3 (`IDX-5`/`IDX-6`), §7.4 (`IDX-7`–`IDX-10`), §7.5 (`IDX-11`–`IDX-13`), §10 (`QRY-1`–`QRY-7`), `DIST-6`, `NFR-4` |

---

## 1. Context

Spec §7 defines the index as a **derived, rebuildable artifact** and specifies required *capabilities*, not a technology. This chunk is the Rust expression of that: two traits (`EmbeddingProvider`, `VectorStore`) plus the reconcile/query engine written **against the traits only**. The concrete backend (sqlite-vec + fastembed) is doc 07; everything here is testable in pure Rust with a mock embedder and in-memory store. A consumer with its own embedding stack (e.g. Craft) implements these traits and ignores the default backend entirely (reference doc §2.1) — the trait boundary is the product of this chunk.

Chunking strategy (spec §7.2 leaves it open; locked here): **one embedding unit per concept**, with the unit text = `description` (if any) + body. Rationale: concept-scale retrieval matches the spec's retrieval model (results are concepts, `IDX-3`), keeps traceability trivial, and avoids a premature chunking algorithm (spec §15 lists canonical chunking as future work). Multi-chunk embedding is a permitted future extension behind `EmbeddingUnit.chunk_ordinal` — the field exists from day one (`IDX-4`).

## 2. Requirements

### 2.1 `EmbeddingProvider` trait (`src/index/mod.rs`)

```rust
pub trait EmbeddingProvider {
    /// Stable identity of the model, e.g. "fastembed/all-MiniLM-L6-v2@4".
    /// Recorded in the index; compared on every open (IDX-5, IDX-12, IDX-15).
    fn model_id(&self) -> &str;
    fn dimensions(&self) -> usize;
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
}
```

- `model_id` must include both model name and a version/revision (`IDX-5`); doc 07's fastembed impl defines the exact format.
- Batch API only (`&[String]`); single-text embeds are a one-element batch. Keeps the trait minimal and lets backends batch efficiently.

### 2.2 Embedding units and metadata

- `pub struct EmbeddingUnit { pub concept: QualifiedConceptId, pub chunk_ordinal: u32, pub text_hash: String, pub vector: Vec<f32>, pub meta: UnitMeta }`
  - `concept` provides traceability to the source concept (`IDX-3`); `chunk_ordinal` is always `0` in v1 (`IDX-4` forward-compat).
  - `text_hash` = SHA-256 of the embedded text; doubles as the staleness signal (`IDX-11`) and the content-change detector (`DIST-6`).
- `pub struct UnitMeta { pub concept_type: Option<String>, pub description: Option<String>, pub tags: Vec<String>, pub language: Option<String>, pub category: Option<String> }` — the structured fields queries filter on (`IDX-9`, `QRY-3`, `STG-4`). Flat and nullable, mirroring the frontmatter accessors from docs 01/03.

### 2.3 `VectorStore` trait

```rust
pub trait VectorStore {
    fn model_id(&self) -> Option<&str>;            // recorded identity, None if uninitialized (IDX-5)
    fn set_model_id(&mut self, id: &str) -> Result<()>;
    fn upsert(&mut self, units: &[EmbeddingUnit]) -> Result<()>;          // IDX-10
    fn remove_concept(&mut self, concept: &QualifiedConceptId) -> Result<()>; // IDX-10
    fn unit_hashes(&self) -> Result<HashMap<QualifiedConceptId, String>>; // staleness diff input (IDX-11)
    fn clear(&mut self) -> Result<()>;                                    // full rebuild (IDX-12)
    fn search(&self, vector: &[f32], k: usize, filter: &Filter) -> Result<Vec<SearchHit>>; // IDX-7..IDX-9
}
```

- `Filter { namespaces: Option<Vec<Namespace>>, argosies: Option<Vec<String>>, concept_types: Option<Vec<String>>, tags: Option<Vec<String>>, language: Option<String>, category: Option<String> }` — every field optional; `None` means unconstrained. This covers namespace scoping (`IDX-8`, `QRY-2`), frontmatter filtering (`IDX-9`, `QRY-3`), and argosy scoping (`QRY-2`/`QRY-6`).
- `SearchHit { concept: QualifiedConceptId, score: f32, meta: UnitMeta }` — score ordering is the store's contract: descending similarity (`IDX-7`).
- `memory/` inclusion: indexing walks whatever namespaces the caller configures (default: `document`, `skill`, `styleguide`, **and** `memory` for a local argosy — memory search is a recommendation in spec §10.2, and `QRY-6` unscoped queries include it when local). Packaging exclusions (`DIST-3`) are unaffected — the index is not distributed (doc 08).

### 2.4 Reconcile engine — `Index<P: EmbeddingProvider, S: VectorStore>`

- `Index::reconcile(&mut self, context: &ProjectContext) -> Result<IndexReport>` — the incremental maintenance loop (`IDX-10`, `NFR-4`, lifecycle §11 steps 3 & 6):
  1. Compare `provider.model_id()` with `store.model_id()` (`IDX-12`): mismatch or store has data with no recorded identity → `store.clear()` + full re-embed + `set_model_id` (**never** a partial rebuild across two models — `IDX-13`; the store can only ever hold one model's vectors by this construction).
  2. Otherwise diff, per active argosy × configured namespace: concept content hash (hash the same text that would be embedded) vs `unit_hashes()`; upsert new/changed concepts, `remove_concept` missing ones. Unchanged concepts cost one hash each — scaling with what changed, not the bundle size (`NFR-4`).
  3. `IndexReport { rebuilt: bool, upserted: usize, removed: usize, unchanged: usize, model_id: String }` — returned for observability, printed by doc 09's `index` subcommand.
- `IDX-1`/`IDX-2` by construction: reconcile's only inputs are the bundle contents and the provider; document that a markdown-only argosy is fully usable after one reconcile.
- Ordering guarantee for `IDX-13`: because a model mismatch always triggers `clear()` before any new vector is stored, mixed-model results are impossible — add a doc comment stating this is the enforcement point of `IDX-13`.

### 2.5 Query API (`QRY-1`–`QRY-7`)

- `Index::search(&self, context, query: &Query) -> Result<Vec<SearchHit>>` with `Query { text: String, k: usize, filter: Filter }`:
  - Embeds `query.text`, calls `store.search` — ranked results (`QRY-1`).
  - `QRY-2`: caller sets `filter.namespaces`/`filter.argosies`; **`argosies` names must be validated against the context's active set** — naming an inactive argosy is `Err`, not an empty result (silent empties hide config mistakes).
  - `QRY-3`: structured fields compose with semantic search in one call (store implementors combine them; doc 07 does it in SQL).
  - `QRY-4` direct lookup is **not** here — it's `ProjectContext::resolve` (doc 05). Link the two in docs so consumers find both retrieval modes.
  - `QRY-6`/`QRY-7`: no `argosies` filter = search everything active; results carry `QualifiedConceptId` (origin visible) and are ordered by `score` alone — **no precedence boosting** (`QRY-7`); enforce by *not threading* precedence information into `search`.

### 2.6 In-memory test doubles (in `#[cfg(test)]` or `index::testing` behind `cfg(any(test, feature = "..."))`)

- `MockEmbedder`: deterministic hash-based vectors (e.g. token-hashing into 128 dims, normalized) with configurable `model_id`; a `with_model_id` constructor to simulate model flips.
- `MemStore`: `HashMap`-backed `VectorStore` with brute-force cosine search honoring all `Filter` fields.

These prove trait sufficiency and let every rule above be unit-tested without ONNX or SQLite.

## 3. Non-Goals

- No SQLite, no fastembed, no persistence format — doc 07 fills the traits.
- No MCP exposure — doc 10.
- No chunking beyond one-unit-per-concept (documented decision, §1).

## 4. Success Criteria

- [ ] Mock-backed tests, one per named behavior:
  - [ ] First reconcile on a fresh context embeds every concept in the default namespaces and records `model_id`; report counts match fixture concept counts (`IDX-1`/`IDX-2`).
  - [ ] Second unchanged reconcile: `unchanged == total`, zero provider embed calls (`NFR-4`, `IDX-10`).
  - [ ] Editing one concept (write via `LocalArgosy` on a temp copy) → next reconcile upserts exactly that concept (`IDX-11`); deleting a concept → `remove_concept` called and hit disappears from search.
  - [ ] Flipping the provider's `model_id` → full rebuild (`rebuilt == true`), old vectors cleared, no mixed results possible (`IDX-12`/`IDX-13`).
  - [ ] `search` with `namespaces: [Styleguide]` returns only styleguide hits (`IDX-8`/`QRY-2`); `language`/`category`/`tags`/`concept_types` filters each narrow correctly (`IDX-9`/`QRY-3`); combined semantic + filter works in one call.
  - [ ] Unscoped search spans local + imported argosys, hits carry origin argosy, ranking is by score (construct fixtures so a local and imported concept have identical text — scores tie, no local-first reordering, `QRY-6`/`QRY-7`).
  - [ ] Scoping to an inactive argosy name errors.
  - [ ] A local argosy's `memory/` concepts are searchable after reconcile; an imported argosy without `memory/` obviously contributes none.
- [ ] Public traits + types are exported from `argosy::index` with doc comments citing requirement IDs.
- [ ] `cargo test` clean with no network, no new runtime dependencies; `fmt`/`clippy` clean.
