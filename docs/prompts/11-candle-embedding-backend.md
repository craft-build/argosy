# 11 — Embedding Backend: candle Provider (replaces fastembed)

| | |
|---|---|
| Depends on | 06 |
| Creates | `src/index/candle.rs`; deletes `src/index/fastembed.rs`; extends `Cargo.toml` (feature `default-index`) |
| Spec sections | §7.3–§7.6 (`IDX-5`, `IDX-12`, `IDX-13`), reference doc §2.1 (no network to be useful), `NFR-1` (portability) |

---

## 1. Context

Doc 07 shipped the default backend as `SqliteVecStore` + `FastembedProvider`. The store stays; the
provider is replaced. Two findings forced the move:

1. fastembed pulls **onnxruntime** (`ort-download-binaries`) — a downloaded C binary wired into every
   build of the `default-index` feature — which is the single least portable link in the chain the
   binary and MCP server actually ship.
2. fastembed also enables `tokenizers`' default **`onig`** feature — the Oniguruma C library — so
   even the tokenization half was not C-free.

The replacement runs the same model natively in Rust through the candle crates. The dependency chain
becomes: `candle-core`/`candle-nn`/`candle-transformers` (pure-Rust CPU math via `gemm`) +
`tokenizers` (`default-features = false`, `fancy-regex` backend) + `hf-hub` (sync `ureq`
downloader; its default TLS is rustls) — **no C dependency anywhere in the embedding chain**, which
is what makes `cargo install argosy` (and the MCP server binary) build on platforms where a
prebuilt onnxruntime cannot be fetched or linked.

**Version pin: candle `0.9`.** candle-core 0.10 introduced a hard, non-optional native-target
dependency on `tokenizers` with the `onig` feature, so 0.9 is the newest line whose default CPU
build stays C-free. Bump only if the C-free property is re-verified (`cargo tree` must not contain
`onig`/`ort`/`onnx`/`native-tls`/`openssl`).

## 2. Requirements

### 2.1 Model spec (static, offline-derivable)

- Model: unchanged from doc 07 — **`sentence-transformers/all-MiniLM-L6-v2`**, 384-dim — so search
  behavior carries over and the vector width still matches every store schema.
- Weights revision: pinned to a full commit sha (`MODEL_REVISION`), so downloads and embeddings are
  reproducible even if the repo's `main` moves. Bump the sha together with
  `CANDLE_BACKEND_VERSION` — the `model_id()` suffix — since either changes the produced vectors.
- `model_id()` format: unchanged in kind from doc 07 — `candle/<repo>@candle-<backend-major>`
  (e.g. `candle/sentence-transformers/all-MiniLM-L6-v2@candle-1`) — derived from constants only.
  `index status` and the MCP startup path never load (or download) the model (`IDX-5`; doc 06's
  offline-identity requirement).
- The identity change vs the old `fastembed/...` ids is deliberate: reconcile's mismatch check
  (`IDX-12`) then clears and rebuilds every existing index once, which is exactly what must happen
  when the embedding function changes (`IDX-13` — never mix vectors across models).

### 2.2 `CandleProvider` / `LazyCandleProvider` (`src/index/candle.rs`)

- Loading: `hf-hub` sync API, cache-first (`ApiRepo::get` resolves from the local cache offline and
  only downloads what is missing) into the shared user cache (§2.3). Fetch `config.json`,
  `tokenizer.json`, `model.safetensors`. Parse the config into
  `candle_transformers::models::bert::Config`; build the tokenizer with `fancy-regex`; load weights
  through `VarBuilder::from_buffered_safetensors` — the buffered, **non-`unsafe`** path — onto
  `Device::Cpu`.
- `embed`: batches of 32 texts; tokenize with truncation at 256 tokens (sentence-transformers'
  `max_seq_length` for this model) and batch-longest padding; `BertModel::forward` with the 0/1
  attention mask (candle converts it to the additive mask internally); then
  **attention-mask-weighted mean pooling + L2 normalization** — the exact post-processing the
  sentence-transformers pipeline applies, and what fastembed produced. One vector per input text,
  in order (doc 06's batch contract).
- Public surface mirrors doc 07's so the CLI/MCP wiring barely moves:
  `CandleProvider::new_default()`, `default_model_id()`, `DEFAULT_DIMENSIONS`, and
  `LazyCandleProvider::new_default()` with the same deferral semantics — instant, offline-tolerant
  open; first `embed` pays the ~90 MB download; failure carries the actionable
  "run `argosy index build` once while online" hint.
- Errors: `Error::Embedding` becomes reason-based (`Embedding { reason: String }`, mirroring
  `Error::Index`) since the error sources are three crates instead of one typed fastembed error.

### 2.3 Model cache

- One shared user-level cache, hf-hub layout, under `$ARGOSY_EMBED_CACHE_DIR` when set (the legacy
  `$FASTEMBED_CACHE_DIR` is still honored so existing scripts keep working), else
  `$XDG_CACHE_HOME/argosy/embeddings`, else `~/.cache/argosy/embeddings` (Windows: under
  `AppData\Local`). Precedence logic stays factored into the pure `cache_dir_from` so it is unit
  tested without the model. Old fastembed ONNX caches simply go unused.

### 2.4 Test strategy (critical — CI has no network)

- Pooling/normalization math is the only nontrivial numeric code: unit-test
  `mean_pool_normalize` on synthetic tensors (padding masked out; unit-norm output) — runs in
  default `cargo test`.
- Cache-dir precedence: same pure-function tests as doc 07.
- Identity: `default_model_id()` format + stability without any model load.
- Exactly one real-model smoke test, `#[ignore]`d (plus the two doc 09 CLI round trips), asserting
  `model_id()` format, 384 dims, and unit-norm output.
- C-free property: not directly unit-testable — asserted by review of `cargo tree` at change time
  (§1).

## 3. Non-Goals

- No GPU/`accelerate`/`mkl` features (they reintroduce native deps); CPU-only by design.
- No model upgrade — the same all-MiniLM-L6-v2 weights, only a new runtime.
- No tokenizer/vocab customization; the repo's `tokenizer.json` is used as-is.
- The bundled C SQLite in `rusqlite` is out of scope: that is the store chain (doc 07), not the
  embedding chain this doc makes C-free.

## 4. Success Criteria

- [x] `cargo tree` (default-index and all-features) contains no `onig`, `ort`, `onnx`, `fastembed`,
      `native-tls`, or `openssl` packages.
- [x] Mock-backed suites (doc 06 traits, doc 07 store, doc 10 MCP) pass unchanged — the trait
      contract is untouched.
- [x] `mean_pool_normalize`, cache-precedence, and identity unit tests pass in default
      `cargo test` (no network).
- [x] The ignored smoke test and both CLI round trips pass with the real model (verify once
      locally with network).
- [x] `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, and
      `cargo test` clean on both CI feature legs.
