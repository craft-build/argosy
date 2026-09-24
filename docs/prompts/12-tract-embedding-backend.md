# 12 — Embedding Backend: tract Provider (replaces candle)

| | |
|---|---|
| Depends on | 06, 11 |
| Creates | `src/index/tract.rs`; deletes `src/index/candle.rs`; extends `Cargo.toml` (feature `default-index`) |
| Spec sections | §7.3–§7.6 (`IDX-5`, `IDX-12`, `IDX-13`), reference doc §2.1 (no network to be useful), `NFR-1` (portability) |

---

## 1. Context

Doc 11 shipped the default backend as `SqliteVecStore` + `CandleProvider`, replacing fastembed to
keep the embedding chain C-free. candle did that, but at a cost: a hand-maintained Rust BERT
reimplementation (`candle-transformers::models::bert`), a `safetensors` weight parser, and a
`tokenizers` copy pulled transitively by `candle-core` with the C `onig` backend (argosy itself
compiles `tokenizers` with `fancy-regex`). It also required trusting that candle's BERT graph matched
the reference implementation numerically.

This doc replaces candle with **`tract-onnx`** — Sonos' pure-Rust inference toolkit — running the
canonical **ONNX** export of the same model. tract parses and optimizes the published graph directly,
so there is no hand-written model code to drift from upstream, and the ONNX file is the exact artifact
`onnxruntime`/fastembed would have consumed. The chain becomes: `tract-onnx` (pure Rust: `prost`,
`nom`, `memmap2`, `rayon`) + `tokenizers` (`default-features = false`, `fancy-regex`) + `hf-hub`
(sync, rustls) — still **no C dependency anywhere in the embedding chain**, and now one fewer
transitive `tokenizers` copy and no `safetensors`.

`tract-transformers` is deliberately **not** used: its `WithTractTransformers` extension is
NNEF-only and its transforms (RoPE, KV cache, SDPA, causal conv) target causal LLMs, so it offers
nothing to a bidirectional MiniLM encoder.

## 2. Requirements

### 2.1 Model spec (static, offline-derivable)

- Model: unchanged from doc 11 — **`sentence-transformers/all-MiniLM-L6-v2`**, 384-dim — so search
  behavior carries over and the vector width still matches every store schema.
- Weights revision: the same pinned commit sha (`MODEL_REVISION`), now resolving
  `onnx/model.onnx` (~90 MB) instead of `model.safetensors`. `config.json` is no longer needed.
- `model_id()` format: `tract/<repo>@tract-<backend-major>` (e.g.
  `tract/sentence-transformers/all-MiniLM-L6-v2@tract-1`) — derived from constants only.
  `index status` and the MCP startup path never load (or download) the model (`IDX-5`).
- The identity change vs `candle/...` is deliberate: reconcile's mismatch check (`IDX-12`) clears and
  rebuilds every existing index once, which is exactly what must happen when the embedding function
  changes (`IDX-13` — never mix vectors across models).

### 2.2 `TractProvider` / `LazyTractProvider` (`src/index/tract.rs`)

- Loading: `hf-hub` sync API, cache-first into the shared user cache (§2.3); fetch `onnx/model.onnx`
  and `tokenizer.json`. Build the tokenizer with `fancy-regex`; then
  `tract_onnx::onnx().model_for_path(..)?.into_optimized()?.into_runnable()?` — parse, optimize, and
  freeze the CPU plan. The graph's batch/sequence axes stay symbolic, so one plan serves every batch
  size and padded length.
- `embed`: batches of 32 texts; tokenize with truncation at 256 tokens (sentence-transformers'
  `max_seq_length` for this model) and batch-longest padding; feed `input_ids`, `attention_mask`, and
  `token_type_ids` as `i64` tensors. The graph's single output is `last_hidden_state`
  `(batch, seq, 384)`; apply **attention-mask-weighted mean pooling + L2 normalization** in plain
  Rust — the exact post-processing the sentence-transformers pipeline applies, and what the previous
  backend produced. One vector per input text, in order (doc 06's batch contract).
- Public surface mirrors doc 11's so the CLI/MCP wiring barely moves:
  `TractProvider::new_default()`, `default_model_id()`, `DEFAULT_DIMENSIONS`, and
  `LazyTractProvider::new_default()` with the same deferral semantics — instant, offline-tolerant
  open; first `embed` pays the ~90 MB download; failure carries the actionable
  "run `argosy index build` once while online" hint.
- Errors: `Error::Embedding` stays reason-based; tract failures are `anyhow::Error` and map through
  the same `embedding_failed` helper.

### 2.3 Model cache

- Unchanged from doc 11: one shared user-level cache, hf-hub layout, under `$ARGOSY_EMBED_CACHE_DIR`
  when set (the legacy `$FASTEMBED_CACHE_DIR` is still honored), else
  `$XDG_CACHE_HOME/argosy/embeddings`, else `~/.cache/argosy/embeddings` (Windows: under
  `AppData\Local`). Precedence logic stays factored into the pure `cache_dir_from` so it is unit
  tested without the model. Old candle `model.safetensors` caches simply go unused; the ONNX model
  lands in the same repo cache directory.

### 2.4 Test strategy (critical — CI has no network)

- Pooling/normalization math is the only nontrivial numeric code: unit-test `mean_pool_normalize` on
  a synthetic hidden state (padding masked out; unit-norm output) — runs in default `cargo test`.
- Cache-dir precedence: same pure-function tests as doc 11.
- Identity: `default_model_id()` format + stability without any model load.
- Exactly one real-model smoke test, `#[ignore]`d (plus the two doc 09 CLI round trips), asserting
  `model_id()` format, 384 dims, and unit-norm output.
- C-free property: not directly unit-testable — asserted by review of `cargo tree` at change time
  (§1).

## 3. Non-Goals

- No model upgrade — the same all-MiniLM-L6-v2 weights, only a new runtime and artifact format.
- No `tract-transformers` (NNEF-only, causal-LLM passes; see §1).
- No quantized/int8 ONNX variant; fp32 `onnx/model.onnx` only (faster imports are a possible
  follow-up).
- No tokenizer/vocab customization; the repo's `tokenizer.json` is used as-is.
- The bundled C SQLite in `rusqlite` is out of scope: that is the store chain (doc 07).

## 4. Success Criteria

- [x] `cargo tree` (default-index and all-features) contains no `onig`, `ort`, `onnx`,
      `safetensors`, `fastembed`, `native-tls`, or `openssl` packages.
- [x] Mock-backed suites (doc 06 traits, doc 07 store, doc 10 MCP) pass unchanged — the trait
      contract is untouched.
- [x] `mean_pool_normalize`, cache-precedence, and identity unit tests pass in default `cargo test`
      (no network).
- [x] The ignored smoke test and both CLI round trips pass with the real model (verified locally).
- [x] `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, the
      feature-matrix `cargo check`s, and `cargo test` clean.
