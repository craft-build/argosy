//! The tract-backed default [`EmbeddingProvider`], gated behind the
//! `default-index` Cargo feature. The model runs natively in Rust through the
//! `tract-onnx` crates — no ONNX runtime shared library; downloads use rustls,
//! and the tokenization this provider performs uses the pure-Rust
//! `fancy-regex` backend, with no live network needed except the first
//! construction, which downloads the ONNX model (~90 MB) into a user-level
//! cache ([`model_cache_dir`]); later runs load from the cache offline.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use snafu::{OptionExt, ResultExt};
use tokenizers::{PaddingParams, Tokenizer, TruncationParams};
use tract::prelude::*;

tract::impl_ndarray_interop!();

use crate::error::{EmbeddingSnafu, IndexSnafu, IoSnafu, Result};

use super::EmbeddingProvider;

/// The Hugging Face repo holding the weights. Mirrors fastembed's prescribed
/// default (`all-MiniLM-L6-v2`, 384-dim) so search behavior carries over
/// unchanged across the backend swap.
const MODEL_REPO: &str = "sentence-transformers/all-MiniLM-L6-v2";

/// The exact weights revision downloaded from [`MODEL_REPO`]. Pinning the
/// commit keeps downloads — and therefore embeddings — reproducible even if
/// the repo's `main` branch moves; bump it (and re-run a full index build,
/// which the identity mismatch below forces) to take a new revision.
const MODEL_REVISION: &str = "1110a243fdf4706b3f48f1d95db1a4f5529b4d41";

/// The ONNX graph file within [`MODEL_REPO`]; tract parses and optimizes this
/// directly. Exported by sentence-transformers and emitting the encoder's
/// `last_hidden_state` `(batch, seq, 384)` — pooling is applied in Rust below,
/// exactly as the sentence-transformers pipeline does.
const MODEL_ONNX_FILE: &str = "onnx/model.onnx";

/// The backend-major token carried in `model_id()` — the tract pin and
/// [`MODEL_REVISION`] move together with it. Numeric drift within the same
/// major is reported identically (mismatch is detected only at this
/// granularity).
const TRACT_BACKEND_VERSION: &str = "1";

/// sentence-transformers' configured `max_seq_length` for this model
/// (`sentence_bert_config.json`); texts are truncated to it.
const MAX_SEQ_TOKENS: usize = 256;

/// Texts per forward pass. Bounds peak memory; MiniLM-L6 is small enough
/// that 32 keeps the CPU busy without ballooning activations.
const EMBED_BATCH: usize = 32;

/// all-MiniLM-L6-v2's vector width (the BERT `hidden_size`).
const MODEL_DIMENSIONS: usize = 384;

/// Maps tract/hf-hub/tokenizer failures into the crate error.
fn embedding_failed(source: impl std::fmt::Display) -> crate::error::Error {
    EmbeddingSnafu {
        reason: source.to_string(),
    }
    .build()
}

/// The stable identity of a model —
/// `tract/<repo>@tract-<backend-major>` (e.g. `...@tract-1`), derived from
/// static metadata only: read-only callers like the CLI's `index status` can
/// compare a store's recorded identity against the current model without
/// loading (or downloading) it.
///
/// A vetted embedding model the default index can use. Selected by name
/// from user configuration ([`ModelSpec::from_name`]); every property is
/// compile-time pinned so embeddings stay reproducible and comparable
/// within one `model_id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ModelSpec {
    /// `sentence-transformers/all-MiniLM-L6-v2` at the pinned revision:
    /// 384-dim, ~90 MB, the long-standing default.
    #[default]
    AllMinilmL6V2,
}

impl ModelSpec {
    /// Every selectable model, for listing in errors and help.
    pub const ALL: &'static [ModelSpec] = &[ModelSpec::AllMinilmL6V2];

    /// The configuration name of this model.
    pub fn name(self) -> &'static str {
        match self {
            ModelSpec::AllMinilmL6V2 => "all-minilm-l6-v2",
        }
    }

    /// The Hugging Face repo holding the weights.
    fn repo(self) -> &'static str {
        match self {
            ModelSpec::AllMinilmL6V2 => MODEL_REPO,
        }
    }

    /// The exact weights revision downloaded from the repo.
    fn revision(self) -> &'static str {
        match self {
            ModelSpec::AllMinilmL6V2 => MODEL_REVISION,
        }
    }

    /// The ONNX graph file within the repo.
    fn onnx_file(self) -> &'static str {
        match self {
            ModelSpec::AllMinilmL6V2 => MODEL_ONNX_FILE,
        }
    }

    /// The model's configured `max_seq_length`; texts truncate to it.
    fn max_seq_tokens(self) -> usize {
        match self {
            ModelSpec::AllMinilmL6V2 => MAX_SEQ_TOKENS,
        }
    }

    /// The vector width every `embed` call produces.
    pub fn dimensions(self) -> usize {
        match self {
            ModelSpec::AllMinilmL6V2 => MODEL_DIMENSIONS,
        }
    }

    /// The stable identity recorded in (and compared against) index
    /// stores — see the backend-version caveat above.
    pub fn model_id(self) -> String {
        format!("tract/{}@tract-{TRACT_BACKEND_VERSION}", self.repo())
    }

    /// Resolves a configuration name to a spec; unknown names are the
    /// caller's error to report (with [`ModelSpec::ALL`] as the valid set).
    pub fn from_name(name: &str) -> Option<Self> {
        ModelSpec::ALL
            .iter()
            .copied()
            .find(|spec| spec.name() == name)
    }
}

/// The tokenizer plus the loaded, runnable ONNX encoder, bundled so the
/// provider can own them under one mutex-guarded slot.
struct Model {
    tokenizer: Tokenizer,
    encoder: Arc<Runnable>,
    dimensions: usize,
}

#[derive(Default)]
struct EmbedTimings {
    tokenize: Duration,
    prepare: Duration,
    infer: Duration,
    pool: Duration,
}

impl Model {
    /// Downloads any missing model files into `cache` (module docs), then
    /// builds the tokenizer and the runnable encoder from the cached files.
    fn load(cache: &std::path::Path, spec: ModelSpec) -> Result<Self> {
        // hf-hub 1.0: the sync client is `HFClientSync` (feature `blocking`);
        // it owns its own background runtime, so calls from inside another
        // tokio runtime (the MCP blocking pool) are safe. The revision is a
        // per-download argument now, not a property of the repo handle.
        let client = hf_hub::HFClient::builder()
            .cache_dir(cache)
            .build_sync()
            .map_err(embedding_failed)?;
        let (owner, name) = hf_hub::split_id(spec.repo());
        let repo = client.model(owner, name);
        // Cache-first lookups (default: `force_download` off): every file
        // resolves offline once downloaded.
        let download = |file: &'static str| {
            repo.download_file()
                .filename(file)
                .revision(spec.revision())
                .send()
                .map_err(embedding_failed)
        };
        let onnx_path = download(spec.onnx_file())?;
        let tokenizer_path = download("tokenizer.json")?;

        let mut tokenizer = Tokenizer::from_file(tokenizer_path).map_err(embedding_failed)?;
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: spec.max_seq_tokens(),
                ..Default::default()
            }))
            .map_err(embedding_failed)?;
        tokenizer.with_padding(Some(PaddingParams::default()));

        // Parse, optimize, and freeze the ONNX graph for the CPU runtime. The
        // graph's batch/sequence axes stay symbolic, so every batch size and
        // padded length runs against the same plan.

        let model = tract::onnx()
            .map_err(embedding_failed)?
            .load(&onnx_path)
            .map_err(embedding_failed)?
            .into_model()
            .map_err(embedding_failed)?;

        cfg_if::cfg_if! {
            if #[cfg(all(target_os = "macos", target_arch = "aarch64"))] {
                let runtime_s = "metal";
            } else {
                let runtime_s = "default";
                tract_linalg::multithread::set_default_executor(
                    tract_linalg::multithread::Executor::multithread(
                        std::thread::available_parallelism()
                            .map(std::num::NonZeroUsize::get)
                            .unwrap_or(1)
                    )
                );
            }
        }
        let runtime = tract::runtime_for_name(runtime_s).map_err(embedding_failed)?;
        let encoder = Arc::new(runtime.prepare(model).map_err(embedding_failed)?);
        Ok(Self {
            tokenizer,
            encoder,
            dimensions: spec.dimensions(),
        })
    }

    /// Embeds one batch: tokenize (truncate to [`MAX_SEQ_TOKENS`], pad to the
    /// batch longest), run the encoder, mean-pool over unmasked token
    /// positions, L2-normalize.
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.embed_timed(texts, None)
    }

    /// Avoid padding short concepts to the longest concept in the incoming
    /// reconcile batch. Keep the returned vectors in their original order;
    /// the index pairs each vector with its source concept by position.
    fn embed_grouped(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        const INFERENCE_BATCH: usize = 8;
        if texts.len() <= INFERENCE_BATCH {
            return self.embed(texts);
        }
        let mut tokenizer = self.tokenizer.clone();
        tokenizer.with_padding(None);
        let lengths = tokenizer
            .encode_batch(texts.iter().map(String::as_str).collect::<Vec<_>>(), true)
            .map_err(embedding_failed)?;
        let mut order: Vec<usize> = (0..texts.len()).collect();
        order.sort_by_key(|&index| lengths[index].len());
        let mut result = vec![Vec::new(); texts.len()];
        for indices in order.chunks(INFERENCE_BATCH) {
            let chunk: Vec<String> = indices.iter().map(|&i| texts[i].clone()).collect();
            for (&index, vector) in indices.iter().zip(self.embed(&chunk)?) {
                result[index] = vector;
            }
        }
        Ok(result)
    }

    fn embed_timed(
        &self,
        texts: &[String],
        mut timings: Option<&mut EmbedTimings>,
    ) -> Result<Vec<Vec<f32>>> {
        let started = timings.as_ref().map(|_| Instant::now());
        let encodings = self
            .tokenizer
            .encode_batch(texts.iter().map(String::as_str).collect::<Vec<_>>(), true)
            .map_err(embedding_failed)?;
        if let (Some(start), Some(t)) = (started, timings.as_deref_mut()) {
            t.tokenize += start.elapsed();
        }
        let batch = encodings.len();
        if batch == 0 {
            return Ok(Vec::new());
        }
        // Padding is enabled above, so every encoding shares one length.
        let seq = encodings[0].get_ids().len();
        let started = timings.as_ref().map(|_| Instant::now());

        let flatten = |pick: fn(&tokenizers::Encoding) -> &[u32]| -> Vec<i64> {
            encodings
                .iter()
                .flat_map(|e| pick(e).iter().map(|&x| x as i64))
                .collect()
        };
        let ids = flatten(|e| e.get_ids());
        let mask = flatten(|e| e.get_attention_mask());
        let type_ids = flatten(|e| e.get_type_ids());

        let input_ids =
            ndarray::Array2::from_shape_vec((batch, seq), ids).map_err(embedding_failed)?;
        let attention_mask = ndarray::Array2::from_shape_vec((batch, seq), mask.clone())
            .map_err(embedding_failed)?;
        let token_type_ids =
            ndarray::Array2::from_shape_vec((batch, seq), type_ids).map_err(embedding_failed)?;

        if let (Some(start), Some(t)) = (started, timings.as_deref_mut()) {
            t.prepare += start.elapsed();
        }
        let started = timings.as_ref().map(|_| Instant::now());
        let outputs = self
            .encoder
            .run([
                input_ids.tract().map_err(embedding_failed)?,
                attention_mask.tract().map_err(embedding_failed)?,
                token_type_ids.tract().map_err(embedding_failed)?,
            ])
            .map_err(embedding_failed)?;
        if let (Some(start), Some(t)) = (started, timings.as_deref_mut()) {
            t.infer += start.elapsed();
        }
        let started = timings.as_ref().map(|_| Instant::now());
        // Single output: `last_hidden_state` `(batch, seq, hidden)`.
        let hidden: &[f32] = outputs[0].as_slice().map_err(embedding_failed)?;
        let hidden: Vec<f32> = hidden.to_vec();

        let mut vectors = Vec::with_capacity(batch);
        let dims = self.dimensions;
        for b in 0..batch {
            let start = b * seq * dims;
            let end = start + seq * dims;
            let m_start = b * seq;
            vectors.push(mean_pool_normalize(
                &hidden[start..end],
                &mask[m_start..m_start + seq],
                seq,
                dims,
            ));
        }
        if let (Some(start), Some(t)) = (started, timings) {
            t.pool += start.elapsed();
        }
        Ok(vectors)
    }
}

/// Sentence-transformers pooling: the attention-mask-weighted mean of the
/// token positions, followed by L2 normalization — the exact post-processing
/// the `all-MiniLM-L6-v2` pipeline applies to the encoder output. `hidden` is
/// `(seq, hidden)` for one sequence, `mask` is `(seq,)` of 0/1; the result is
/// `(hidden,)`.
fn mean_pool_normalize(hidden: &[f32], mask: &[i64], seq: usize, dims: usize) -> Vec<f32> {
    let mut pooled = vec![0f32; dims];
    let mut count = 0f32;
    for t in 0..seq {
        if mask[t] == 0 {
            continue;
        }
        count += 1.0;
        let row = &hidden[t * dims..(t + 1) * dims];
        for (acc, value) in pooled.iter_mut().zip(row) {
            *acc += value;
        }
    }
    for value in pooled.iter_mut() {
        *value /= count;
    }
    let norm = pooled.iter().map(|x| x * x).sum::<f32>().sqrt();
    for value in pooled.iter_mut() {
        *value /= norm;
    }
    pooled
}

/// A local tract [`EmbeddingProvider`]: BERT text embeddings with no remote
/// service (see module docs for the dependency posture and the
/// first-run-download tolerance).
pub struct TractProvider {
    model: Mutex<Model>,
    model_id: String,
    dimensions: usize,
}

impl TractProvider {
    /// The vector width every `embed` call produces (all-MiniLM-L6-v2).
    pub const DEFAULT_DIMENSIONS: usize = MODEL_DIMENSIONS;

    /// The model identity [`TractProvider::new_default`] will report,
    /// without constructing (or downloading) the model.
    pub fn default_model_id() -> String {
        ModelSpec::default().model_id()
    }

    /// Creates a provider over the pinned default model. Downloads the model
    /// on first use (module docs).
    pub fn new_default() -> Result<Self> {
        Self::new_default_in(None)
    }

    /// [`TractProvider::new_default`] with an explicit model-cache
    /// directory (hosts pass a configured path); `None` resolves the cache
    /// from the environment ([`model_cache_dir`]).
    pub fn new_default_in(cache: Option<&Path>) -> Result<Self> {
        Self::new(ModelSpec::default(), cache)
    }

    /// Creates a provider over a selected [`ModelSpec`], with the same
    /// cache resolution as [`TractProvider::new_default_in`].
    pub fn new(spec: ModelSpec, cache: Option<&Path>) -> Result<Self> {
        // `$ARGOSY_EMBED_CACHE_DIR` / `$FASTEMBED_CACHE_DIR` outrank the
        // caller's explicit path: an env-pinned cache must never be
        // silently relocated by configuration.
        let cache = if env_cache_dir().is_some() {
            model_cache_dir()?
        } else {
            match cache {
                Some(dir) => dir.to_path_buf(),
                None => model_cache_dir()?,
            }
        };
        // Create before the download touches it: a clear, early error for an
        // unwritable cache location instead of a mid-download failure.
        fs::create_dir_all(&cache).context(IoSnafu {
            path: cache.clone(),
        })?;
        let model = Mutex::new(Model::load(&cache, spec)?);
        Ok(Self {
            model,
            model_id: spec.model_id(),
            dimensions: spec.dimensions(),
        })
    }
}

/// Where the model weights are cached. One shared user-level cache:
/// `$ARGOSY_EMBED_CACHE_DIR` when set (the now-legacy `$FASTEMBED_CACHE_DIR`
/// is still honored), else `$XDG_CACHE_HOME/argosy/embeddings` (falling back
/// to `~/.cache/argosy/embeddings`; on Windows the per-user cache lives
/// under `AppData\Local`).
pub fn model_cache_dir() -> Result<PathBuf> {
    cache_dir_from(
        env_cache_dir(),
        std::env::var_os("XDG_CACHE_HOME"),
        std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")),
    )
}

/// The env-var cache override, when set.
fn env_cache_dir() -> Option<OsString> {
    std::env::var_os("ARGOSY_EMBED_CACHE_DIR").or_else(|| std::env::var_os("FASTEMBED_CACHE_DIR"))
}

/// Pure core of [`model_cache_dir`], env reads factored out for tests.
fn cache_dir_from(
    embed_cache: Option<OsString>,
    xdg_cache_home: Option<OsString>,
    home: Option<OsString>,
) -> Result<PathBuf> {
    if let Some(dir) = embed_cache
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
    {
        return Ok(dir);
    }
    let base = xdg_cache_home
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        // `~/.cache` on Unix; on Windows `home` arrives via USERPROFILE
        // (HOME is unset there) and the per-user cache is AppData\Local.
        .or_else(|| {
            home.map(|home| {
                let home = PathBuf::from(home);
                if cfg!(windows) {
                    home.join("AppData").join("Local")
                } else {
                    home.join(".cache")
                }
            })
        })
        .context(IndexSnafu {
            reason: "cannot locate the embedding-model cache: set ARGOSY_EMBED_CACHE_DIR, \
                     XDG_CACHE_HOME, or HOME"
                .to_string(),
        })?;
    Ok(base.join("argosy").join("embeddings"))
}

/// A [`TractProvider`] that defers model construction — and the first-run
/// ~90 MB download — to the first [`EmbeddingProvider::embed`] call. Identity
/// and dimensionality derive from static metadata, so hash-diff previews
/// never load the model and a serving process starts instantly;
/// embedding-dependent ops fail with an actionable hint.
pub struct LazyTractProvider {
    model_id: String,
    dimensions: usize,
    model: Mutex<Option<TractProvider>>,
    spec: ModelSpec,
    cache: Option<PathBuf>,
}

impl LazyTractProvider {
    /// A lazy provider over the pinned default model. Never downloads or
    /// constructs anything until the first `embed`.
    pub fn new_default() -> Result<Self> {
        Self::new_default_in(None)
    }

    /// [`LazyTractProvider::new_default`] with an explicit model-cache
    /// directory used at first-`embed` load time.
    pub fn new_default_in(cache: Option<PathBuf>) -> Result<Self> {
        Self::new(ModelSpec::default(), cache)
    }

    /// A lazy provider over a selected [`ModelSpec`].
    pub fn new(spec: ModelSpec, cache: Option<PathBuf>) -> Result<Self> {
        Ok(Self {
            model_id: spec.model_id(),
            dimensions: spec.dimensions(),
            model: Mutex::new(None),
            spec,
            cache,
        })
    }
}

impl EmbeddingProvider for LazyTractProvider {
    fn model_id(&self) -> &str {
        // Static metadata: no model load, works offline.
        &self.model_id
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let mut slot = self.model.lock().map_err(|_| -> crate::error::Error {
            IndexSnafu {
                reason: "embedding model mutex poisoned by a panicking caller".to_string(),
            }
            .build()
        })?;
        if slot.is_none() {
            let provider =
                TractProvider::new(self.spec, self.cache.as_deref()).map_err(|source| {
                    IndexSnafu {
                        reason: format!(
                            "embedding model unavailable: {source}; run `argosy index build` \
                         once while online to download it (~90 MB), then retry"
                        ),
                    }
                    .build()
                })?;
            *slot = Some(provider);
        }
        slot.as_ref().expect("populated above").embed(texts)
    }
}

impl EmbeddingProvider for TractProvider {
    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let model = self.model.lock().map_err(|_| -> crate::error::Error {
            IndexSnafu {
                reason: "embedding model mutex poisoned by a panicking caller".to_string(),
            }
            .build()
        })?;
        let mut vectors = Vec::with_capacity(texts.len());
        for batch in texts.chunks(EMBED_BATCH) {
            vectors.extend(model.embed_grouped(batch)?);
        }
        Ok(vectors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reproducible opt-in inference microbenchmark. Example:
    /// `ARGOSY_BENCH_DOCS=.lab/workspace/.../document ARGOSY_BENCH_BATCH=16
    ///  ARGOSY_BENCH_GROUP=1 cargo test --release --lib
    ///  index::tract::tests::embedding_batch_benchmark -- --ignored --nocapture`
    /// Pair with `/usr/bin/time -l` on macOS to capture peak resident memory.
    #[test]
    #[ignore = "loads model; explicitly opt in for inference benchmarking"]
    fn embedding_batch_benchmark() {
        let docs = std::env::var("ARGOSY_BENCH_DOCS").expect("set ARGOSY_BENCH_DOCS");
        let batch: usize = std::env::var("ARGOSY_BENCH_BATCH")
            .unwrap_or_else(|_| "32".into())
            .parse()
            .unwrap();
        assert!((1..=32).contains(&batch));
        let group = std::env::var("ARGOSY_BENCH_GROUP").is_ok_and(|s| s == "1");
        let repetitions: usize = std::env::var("ARGOSY_BENCH_REPS")
            .unwrap_or_else(|_| "5".into())
            .parse()
            .unwrap();
        assert!(repetitions > 0);
        let mut paths: Vec<_> = fs::read_dir(docs)
            .unwrap()
            .map(|item| item.unwrap().path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "md"))
            .collect();
        paths.sort();
        let texts: Vec<String> = paths
            .iter()
            .map(|path| {
                let file = fs::read_to_string(path).unwrap();
                file.splitn(3, "---\n").nth(2).unwrap().to_string()
            })
            .collect();

        let start = Instant::now();
        let model = Model::load(&model_cache_dir().unwrap(), ModelSpec::default()).unwrap();
        let load = start.elapsed();
        let reference: Vec<Vec<f32>> = texts
            .chunks(32)
            .flat_map(|chunk| model.embed(chunk).unwrap())
            .collect();
        let production: Vec<Vec<f32>> = texts
            .chunks(EMBED_BATCH)
            .flat_map(|chunk| model.embed_grouped(chunk).unwrap())
            .collect();
        for (actual, expected) in production.iter().zip(&reference) {
            let cosine: f32 = actual.iter().zip(expected).map(|(a, b)| a * b).sum();
            assert!(cosine > 0.9999, "provider must preserve vector order");
        }
        let mut timing = EmbedTimings::default();
        let mut total = Duration::ZERO;
        let mut grouped_time = Duration::ZERO;
        let mut minimum_cosine = 1.0f32;
        for _ in 0..repetitions {
            let start = Instant::now();
            let mut order: Vec<usize> = (0..texts.len()).collect();
            if group {
                let mut tokenizer = model.tokenizer.clone();
                tokenizer.with_padding(None);
                let lengths = tokenizer
                    .encode_batch(texts.iter().map(String::as_str).collect::<Vec<_>>(), true)
                    .unwrap();
                order.sort_by_key(|&i| lengths[i].len());
            }
            grouped_time += start.elapsed();
            let start = Instant::now();
            let mut result = vec![Vec::new(); texts.len()];
            for indices in order.chunks(batch) {
                let chunk: Vec<String> = indices.iter().map(|&i| texts[i].clone()).collect();
                let vectors = model.embed_timed(&chunk, Some(&mut timing)).unwrap();
                for (&index, vector) in indices.iter().zip(vectors) {
                    result[index] = vector;
                }
            }
            total += start.elapsed();
            for (actual, expected) in result.iter().zip(&reference) {
                let cosine: f32 = actual.iter().zip(expected).map(|(a, b)| a * b).sum();
                minimum_cosine = minimum_cosine.min(cosine);
            }
        }
        println!(
            "BENCH batch={batch} group={group} concepts={} reps={repetitions} \
             load_ms={:.2} group_ms={:.2} embed_ms={:.2} tokenize_ms={:.2} \
             prepare_ms={:.2} infer_ms={:.2} pool_ms={:.2} min_cosine={minimum_cosine:.7}",
            texts.len(),
            load.as_secs_f64() * 1e3,
            grouped_time.as_secs_f64() * 1e3 / repetitions as f64,
            total.as_secs_f64() * 1e3 / repetitions as f64,
            timing.tokenize.as_secs_f64() * 1e3 / repetitions as f64,
            timing.prepare.as_secs_f64() * 1e3 / repetitions as f64,
            timing.infer.as_secs_f64() * 1e3 / repetitions as f64,
            timing.pool.as_secs_f64() * 1e3 / repetitions as f64,
        );
        assert!(minimum_cosine > 0.9999, "embedding drift after regrouping");
    }

    // /// Compare an alternate graph or truncation limit against the default
    // /// provider on a small corpus. This is a smoke test, not a quality gate.
    // #[test]
    // #[ignore = "set ARGOSY_BENCH_ONNX or ARGOSY_BENCH_TRUNCATE"]
    // fn candidate_onnx_smoke_test() {
    //     let path = std::env::var("ARGOSY_BENCH_ONNX").ok();
    //     let truncation = std::env::var("ARGOSY_BENCH_TRUNCATE")
    //         .ok()
    //         .map(|value| value.parse::<usize>().unwrap());
    //     assert!(
    //         path.is_some() ^ truncation.is_some(),
    //         "choose one candidate"
    //     );
    //     let baseline = Model::load(&model_cache_dir().unwrap()).unwrap();
    //     let start = Instant::now();
    //     let encoder = match &path {
    //         Some(path) => tract_onnx::onnx()
    //             .model_for_path(path)
    //             .unwrap()
    //             .into_optimized()
    //             .unwrap()
    //             .into_runnable()
    //             .unwrap(),
    //         None => baseline.encoder.clone(),
    //     };
    //     let preparation = start.elapsed();
    //     let mut alternate = Model {
    //         tokenizer: baseline.tokenizer.clone(),
    //         encoder,
    //     };
    //     if let Some(max_length) = truncation {
    //         assert!(max_length > 0 && max_length < MAX_SEQ_TOKENS);
    //         alternate
    //             .tokenizer
    //             .with_truncation(Some(TruncationParams {
    //                 max_length,
    //                 ..Default::default()
    //             }))
    //             .unwrap();
    //     }
    //     let docs = std::env::var("ARGOSY_BENCH_DOCS").expect("set ARGOSY_BENCH_DOCS");
    //     let mut paths: Vec<_> = fs::read_dir(docs)
    //         .unwrap()
    //         .map(|item| item.unwrap().path())
    //         .filter(|path| path.extension().is_some_and(|extension| extension == "md"))
    //         .collect();
    //     paths.sort();
    //     let texts: Vec<_> = paths
    //         .iter()
    //         .map(|path| {
    //             fs::read_to_string(path)
    //                 .unwrap()
    //                 .splitn(3, "---\n")
    //                 .nth(2)
    //                 .unwrap()
    //                 .to_string()
    //         })
    //         .collect();
    //     let reference = baseline.embed_grouped(&texts).unwrap();
    //     let start = Instant::now();
    //     let vectors = alternate.embed_grouped(&texts).unwrap();
    //     let elapsed = start.elapsed();
    //     let minimum_cosine = vectors
    //         .iter()
    //         .zip(&reference)
    //         .map(|(actual, expected)| actual.iter().zip(expected).map(|(a, b)| a * b).sum::<f32>())
    //         .fold(1.0f32, f32::min);
    //     let nearest = |all: &[Vec<f32>], index: usize| {
    //         let mut ranked: Vec<_> = (0..all.len()).filter(|&i| i != index).collect();
    //         ranked.sort_by(|&a, &b| {
    //             let score = |i: usize| {
    //                 all[index]
    //                     .iter()
    //                     .zip(&all[i])
    //                     .map(|(x, y)| x * y)
    //                     .sum::<f32>()
    //             };
    //             score(b).total_cmp(&score(a)).then_with(|| a.cmp(&b))
    //         });
    //         ranked.truncate(5);
    //         ranked
    //     };
    //     let overlap: usize = (0..texts.len())
    //         .map(|i| {
    //             let actual = nearest(&vectors, i);
    //             let expected = nearest(&reference, i);
    //             actual.iter().filter(|item| expected.contains(item)).count()
    //         })
    //         .sum();
    //     let top5_overlap = overlap as f64 / (texts.len() * 5) as f64;
    //     println!(
    //         "CANDIDATE path={} truncation={:?} concepts={} prepare_ms={:.2} embed_ms={:.2} \
    //          min_cosine={minimum_cosine:.7} top5_overlap={top5_overlap:.4}",
    //         path.as_deref().unwrap_or("default"),
    //         truncation,
    //         texts.len(),
    //         preparation.as_secs_f64() * 1e3,
    //         elapsed.as_secs_f64() * 1e3
    //     );
    // }

    #[test]
    fn cache_dir_precedence_and_fallbacks() {
        // An explicit `$ARGOSY_EMBED_CACHE_DIR` wins and is honored verbatim.
        assert_eq!(
            cache_dir_from(
                Some("/ec".into()),
                Some("/xdg".into()),
                Some("/home".into())
            )
            .unwrap(),
            PathBuf::from("/ec")
        );
        // XDG next, namespaced under argosy/embeddings.
        assert_eq!(
            cache_dir_from(None, Some("/xdg".into()), Some("/home".into())).unwrap(),
            PathBuf::from("/xdg/argosy/embeddings")
        );
        // HOME fallback when XDG_CACHE_HOME is unset.
        assert_eq!(
            cache_dir_from(None, None, Some("/home".into())).unwrap(),
            PathBuf::from("/home/.cache/argosy/embeddings")
        );
        // Empty strings count as unset (mirrors `global_argosy_dir`).
        assert_eq!(
            cache_dir_from(None, Some("".into()), Some("/home".into())).unwrap(),
            PathBuf::from("/home/.cache/argosy/embeddings")
        );
        assert_eq!(
            cache_dir_from(Some("".into()), None, Some("/home".into())).unwrap(),
            PathBuf::from("/home/.cache/argosy/embeddings")
        );
        // Nothing to derive from: an actionable error, never a CWD-relative cache.
        assert!(cache_dir_from(None, None, None).is_err());
    }

    #[test]
    fn mean_pooling_masks_padding_and_normalizes() {
        // Two positions of a 2-dim hidden state; the second is padding.
        let hidden = [1.0f32, 1.0, 2.0, 2.0];
        let mask = [1i64, 0];
        let out = mean_pool_normalize(&hidden, &mask, 2, 2);
        // Masked mean [1, 1], L2-normalized to 1/sqrt(2).
        let unit = 1.0 / 2.0f32.sqrt();
        assert!((out[0] - unit).abs() < 1e-6 && (out[1] - unit).abs() < 1e-6);

        // All positions unmasked: mean [0, 2/3], L2-normalized to [0, 1].
        let hidden = [-1.0f32, 1.0, 0.0, 0.0, 1.0, 1.0];
        let mask = [1i64, 1, 1];
        let out = mean_pool_normalize(&hidden, &mask, 3, 2);
        assert!(out[0].abs() < 1e-6 && (out[1] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn model_identity_is_offline_derivable_and_stable() {
        assert_eq!(
            TractProvider::default_model_id(),
            ModelSpec::default().model_id()
        );
        let id = ModelSpec::default().model_id();
        assert!(
            id.starts_with("tract/") && id.contains("all-MiniLM-L6-v2") && id.contains("@tract-"),
            "model_id() follows tract/<model>@tract-<major>: {id}"
        );
    }

    /// The single model test: needs network on a cold model cache, so it
    /// never runs in default `cargo test`.
    #[test]
    #[ignore = "downloads the model weights; run with --ignored"]
    fn default_model_embeds_384_dims_and_reports_a_stable_identity() {
        let a = TractProvider::new_default().unwrap();
        let b = TractProvider::new_default().unwrap();
        assert_eq!(
            a.model_id(),
            b.model_id(),
            "IDX-5: two instances of the same model report identical ids"
        );
        assert_eq!(a.dimensions(), 384);

        let vectors = a.embed(&["borrow checker basics".to_string()]).unwrap();
        assert_eq!(vectors.len(), 1);
        assert_eq!(vectors[0].len(), 384);
        // Normalized output: unit L2 norm (sentence-transformers pipelines
        // normalize, and the cosine search on top of it assumes it).
        let norm: f32 = vectors[0].iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!(
            (norm - 1.0).abs() < 1e-3,
            "expected a unit vector, got {norm}"
        );
    }
}
