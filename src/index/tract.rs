//! The tract-backed default [`EmbeddingProvider`], gated behind the
//! `default-index` Cargo feature. The model runs natively in Rust through the
//! `tract-onnx` crates — no ONNX runtime shared library; downloads use rustls,
//! and the tokenization this provider performs uses the pure-Rust
//! `fancy-regex` backend, with no live network needed except the first
//! construction, which downloads the ONNX model (~90 MB) into a user-level
//! cache ([`model_cache_dir`]); later runs load from the cache offline.

use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use snafu::{OptionExt, ResultExt};
use tokenizers::{PaddingParams, Tokenizer, TruncationParams};
use tract_onnx::prelude::{
    Framework, InferenceModelExt, IntoRunnable, IntoTensor, TypedRunnableModel, tract_ndarray, tvec,
};

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

/// The stable identity of the default model —
/// `tract/<repo>@tract-<backend-major>` (e.g. `...@tract-1`), derived from
/// static metadata only: read-only callers like the CLI's `index status` can
/// compare a store's recorded identity against the current default without
/// loading (or downloading) the model.
fn model_id() -> String {
    format!("tract/{MODEL_REPO}@tract-{TRACT_BACKEND_VERSION}")
}

/// The tokenizer plus the loaded, runnable ONNX encoder, bundled so the
/// provider can own them under one mutex-guarded slot.
struct Model {
    tokenizer: Tokenizer,
    encoder: Arc<TypedRunnableModel>,
}

impl Model {
    /// Downloads any missing model files into `cache` (module docs), then
    /// builds the tokenizer and the runnable encoder from the cached files.
    fn load(cache: &std::path::Path) -> Result<Self> {
        // hf-hub 1.0: the sync client is `HFClientSync` (feature `blocking`);
        // it owns its own background runtime, so calls from inside another
        // tokio runtime (the MCP blocking pool) are safe. The revision is a
        // per-download argument now, not a property of the repo handle.
        let client = hf_hub::HFClient::builder()
            .cache_dir(cache)
            .build_sync()
            .map_err(embedding_failed)?;
        let (owner, name) = hf_hub::split_id(MODEL_REPO);
        let repo = client.model(owner, name);
        // Cache-first lookups (default: `force_download` off): every file
        // resolves offline once downloaded.
        let download = |file: &'static str| {
            repo.download_file()
                .filename(file)
                .revision(MODEL_REVISION)
                .send()
                .map_err(embedding_failed)
        };
        let onnx_path = download(MODEL_ONNX_FILE)?;
        let tokenizer_path = download("tokenizer.json")?;

        let mut tokenizer = Tokenizer::from_file(tokenizer_path).map_err(embedding_failed)?;
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: MAX_SEQ_TOKENS,
                ..Default::default()
            }))
            .map_err(embedding_failed)?;
        tokenizer.with_padding(Some(PaddingParams::default()));

        // Parse, optimize, and freeze the ONNX graph for the CPU runtime. The
        // graph's batch/sequence axes stay symbolic, so every batch size and
        // padded length runs against the same plan.
        let encoder = tract_onnx::onnx()
            .model_for_path(&onnx_path)
            .map_err(embedding_failed)?
            .into_optimized()
            .map_err(embedding_failed)?
            .into_runnable()
            .map_err(embedding_failed)?;
        Ok(Self { tokenizer, encoder })
    }

    /// Embeds one batch: tokenize (truncate to [`MAX_SEQ_TOKENS`], pad to the
    /// batch longest), run the encoder, mean-pool over unmasked token
    /// positions, L2-normalize.
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let encodings = self
            .tokenizer
            .encode_batch(texts.iter().map(String::as_str).collect::<Vec<_>>(), true)
            .map_err(embedding_failed)?;
        let batch = encodings.len();
        if batch == 0 {
            return Ok(Vec::new());
        }
        // Padding is enabled above, so every encoding shares one length.
        let seq = encodings[0].get_ids().len();

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
            tract_ndarray::Array2::from_shape_vec((batch, seq), ids).map_err(embedding_failed)?;
        let attention_mask = tract_ndarray::Array2::from_shape_vec((batch, seq), mask.clone())
            .map_err(embedding_failed)?;
        let token_type_ids = tract_ndarray::Array2::from_shape_vec((batch, seq), type_ids)
            .map_err(embedding_failed)?;

        let outputs = self
            .encoder
            .run(tvec!(
                input_ids.into_tensor().into(),
                attention_mask.into_tensor().into(),
                token_type_ids.into_tensor().into()
            ))
            .map_err(embedding_failed)?;
        // Single output: `last_hidden_state` `(batch, seq, hidden)`.
        let hidden = outputs[0]
            .to_plain_array_view::<f32>()
            .map_err(embedding_failed)?;
        let hidden: Vec<f32> = hidden.iter().copied().collect();

        let mut vectors = Vec::with_capacity(batch);
        for b in 0..batch {
            let start = b * seq * MODEL_DIMENSIONS;
            let end = start + seq * MODEL_DIMENSIONS;
            let m_start = b * seq;
            vectors.push(mean_pool_normalize(
                &hidden[start..end],
                &mask[m_start..m_start + seq],
                seq,
                MODEL_DIMENSIONS,
            ));
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
        model_id()
    }

    /// Creates a provider over the pinned default model. Downloads the model
    /// on first use (module docs).
    pub fn new_default() -> Result<Self> {
        let cache = model_cache_dir()?;
        // Create before the download touches it: a clear, early error for an
        // unwritable cache location instead of a mid-download failure.
        fs::create_dir_all(&cache).context(IoSnafu {
            path: cache.clone(),
        })?;
        let model = Mutex::new(Model::load(&cache)?);
        Ok(Self {
            model,
            model_id: model_id(),
            dimensions: MODEL_DIMENSIONS,
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
        std::env::var_os("ARGOSY_EMBED_CACHE_DIR")
            .or_else(|| std::env::var_os("FASTEMBED_CACHE_DIR")),
        std::env::var_os("XDG_CACHE_HOME"),
        std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")),
    )
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
}

impl LazyTractProvider {
    /// A lazy provider over the pinned default model. Never downloads or
    /// constructs anything until the first `embed`.
    pub fn new_default() -> Result<Self> {
        Ok(Self {
            model_id: TractProvider::default_model_id(),
            dimensions: TractProvider::DEFAULT_DIMENSIONS,
            model: Mutex::new(None),
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
            let provider = TractProvider::new_default().map_err(|source| {
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
            vectors.extend(model.embed(batch)?);
        }
        Ok(vectors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(TractProvider::default_model_id(), model_id());
        let id = model_id();
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
