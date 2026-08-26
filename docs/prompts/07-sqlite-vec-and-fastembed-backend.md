# 07 — Default Index Backend: sqlite-vec Store + fastembed Provider

| | |
|---|---|
| Depends on | 06 |
| Creates | `src/index/sqlite.rs`, `src/index/fastembed.rs`; extends `Cargo.toml` (feature `default-index`) |
| Spec sections | §7.3–§7.6 (`IDX-5`–`IDX-16`), §3.4 (an argosy is a directory, not a service), `DIST-6`, reference doc §6 (store decision) |

---

## 1. Context

Doc 06 defined the capability traits and the reconcile/query engine with in-memory doubles. This chunk ships the **default backend** the binary and MCP server actually run (locked decisions, doc 00 §3): `sqlite-vec` for the store — one `.argosy/index.db` file beside the markdown, SQL covering the `IDX-9` structured-filter half — and `fastembed` (local ONNX) for embeddings, satisfying "no network dependency required to be useful out of the box" (reference doc §2.1). Both live behind the doc 06 traits and the `default-index` Cargo feature (default on), so library consumers with their own stack pay nothing.

## 2. Requirements

### 2.1 Where the index lives

- Default path: `<project-root>/.argosy/index.db` — derived artifact next to the bundle, per the locked decision (doc 00 §3). `SqliteVecStore::open(path)` creates parent dirs and the schema on first use.
- The index directory is **not bundle content**: doc 02 already ignores `.argosy/` during validation/walking; doc 08 packaging excludes it. Restate this in module docs.
- One `.db` file per `ProjectContext` spans all active argosys (local + imported in one context); `QualifiedConceptId.argosy` separates their rows (`MUL-5`).

### 2.2 `SqliteVecStore` (`src/index/sqlite.rs`)

Dependencies: `rusqlite` (feature `bundled`), `sqlite-vec` (extension loading — use the crate's bundled static linking if offered; loadable-extension fallback must produce a clear error, not a panic).

- Schema (v1, with a `PRAGMA user_version` for future migrations):
  - meta table: single row holding `model_id` (`IDX-5`), `dimensions`, created/updated timestamps.
  - units table: `argosy TEXT, namespace TEXT, concept_id TEXT, chunk_ordinal INTEGER, text_hash TEXT, concept_type TEXT, description TEXT, tags TEXT /* JSON array */, language TEXT, category TEXT`, PK `(argosy, namespace, concept_id, chunk_ordinal)`.
  - `vec0` virtual table `unit_vectors(vector float[N])` keyed by the units rowid (`sqlite-vec` doesn't support metadata columns in the vec table — the join to `units` is the filtering layer; that join **is** the `IDX-9` implementation).
- Trait impl mapping:
  - `model_id`/`set_model_id` → meta table.
  - `upsert` → delete-then-insert per unit inside one transaction (vec0 has no UPDATE); batch the caller's slice in one transaction for speed.
  - `remove_concept` → delete units rows + their vec rows by rowid.
  - `unit_hashes` → `SELECT` of the PK + `text_hash` into the `HashMap<QualifiedConceptId, String>` the reconcile engine diffs (`IDX-11`).
  - `clear` → drop/re-create data tables, keep meta (then reconcile re-stamps `model_id`).
  - `search` → embed-side: `vector` is matched in the vec0 KNN query with join to `units`, applying `Filter` fields as `WHERE` clauses built from parameters (never string-interpolate user values; namespaces/argosies/tags/type/language/category all bound parameters) (`IDX-7`/`IDX-8`/`IDX-9`). `k` maps to vec0's `k = ?` constraint. Score surfaced as `1 - distance` or distance negated — pick, document, keep it monotonic with similarity (`SearchHit.score` is descending-similarity per doc 06).
- Dimension safety: vec dimension fixed at table creation from the provider's `dimensions()`; a vectors-of-wrong-length upsert errors — don't silently truncate.
- Concurrency: one writer at a time is fine (MCP/CLI are single-process); open with normal journal mode WAL for crash safety. No multi-process guarantees — document that.

### 2.3 `FastembedProvider` (`src/index/fastembed.rs`)

Dependency: `fastembed` (default features off; enable only the text-embedding pieces needed — keep compile time and tree size down; justify chosen feature flags in `Cargo.toml` comments).

- Model: `fastembed::TextEmbedding` with a small ONNX model — **`AllMiniLML6V2`** (384-dim) is the prescribed default; expose `FastembedProvider::new_default()` plus `with_model(EmbeddingModel)` for callers.
- `model_id()` format: `fastembed/<model-name>@<model-revision-or-crate-version>` — must be stable across runs and change when the model changes (`IDX-5`); verify in a test that two instances report identical ids.
- `embed`: map directly onto `TextEmbedding::embed`; convert fastembed errors into `Error::Embedding`-style variant (add to `error.rs`).
- First run downloads the model into fastembed's cache — that's the **only** tolerated network access in the crate, and it must be documented on `FastembedProvider` and in the CLI `index` command help (doc 09).

### 2.4 Precomputed embeddings in distribution (`IDX-14`–`IDX-16`)

- `PackageOptions` in doc 08 can opt to include `<argosy>/.argosy/index.db` as an optimization (`IDX-14`, `IDX-16`: never authoritative).
- This chunk's part: `SqliteVecStore::open` already records/checks nothing beyond `model_id` — the **model-match check is reconcile's** (`IDX-12`/`IDX-15`); a harness with a different provider model id rebuilds automatically. Add one explicit test: open a store built with `MockEmbedder{model A}`, reconcile with `FastembedProvider`-shaped different id → full rebuild, no error, no mixed vectors (`IDX-15`).

### 2.5 Test strategy (critical — CI has no network)

- All sqlite-vec behavior is tested with `MockEmbedder` from doc 06 against a **real** `SqliteVecStore` on `tempfile` dbs: this is where vec0 SQL, filter SQL, and transactions get coverage.
- Exactly one fastembed smoke test, marked `#[ignore = "downloads ONNX model; run with --ignored"]`, asserting `model_id()` format and `embed` output dimensions == 384. It never runs in default `cargo test`.
- A `reconcile_end_to_end_with_sqlite_store` integration test ties doc 06 engine + doc 07 store on a temp ProjectContext: reconcile → search → edit one concept → reconcile → result set changes as expected; then flip model id → full rebuild (`IDX-12`/`IDX-15`).

## 3. Non-Goals

- No CLI wiring (doc 09), no MCP (doc 10).
- No multi-chunk vectors; no alternate distance metrics beyond what vec0's default gives.
- No encryption/access control on the db file (spec §12.3 out of scope).

## 4. Success Criteria

- [ ] `cargo build --no-default-features` (library without backend) compiles — traits usable standalone; `cargo build --features default-index` compiles with the backend.
- [ ] Mock-embedder + real-sqlite tests pass for every `VectorStore` method, including: upsert-then-search retrieval ordering sanity (nearest text first), namespace/argosy/type/tags/language/category `WHERE` filtering, `remove_concept`, `unit_hashes` round-trip, `clear`, wrong-dimension upsert errors, `user_version` set.
- [ ] `reconcile_end_to_end_with_sqlite_store` (see §2.5) passes, including the model-mismatch full-rebuild branch (`IDX-12`/`IDX-15`/`IDX-13`).
- [ ] fastembed smoke test exists, is `#[ignore]`d, and passes with `cargo test -- --ignored` on a machine with network (verify once locally, note result in the PR/commit message — do not block CI on it).
- [ ] Opening a second `ProjectContext` over the same project root reuses the existing `index.db` with zero re-embeds (persisted-model fast path).
- [ ] `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` clean (default and `--no-default-features`).
