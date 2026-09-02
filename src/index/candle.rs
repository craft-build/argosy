//! The candle-backed default [`EmbeddingProvider`], gated behind the
//! `default-index` Cargo feature. The model runs natively in Rust through the
//! candle crates — no ONNX runtime and no C dependency anywhere in the chain
//! (tokenization uses the pure-Rust `fancy-regex` backend, downloads use
//! rustls) — with no live network needed except the first construction, which
//! downloads the model weights (~90 MB) into a user-level cache
//! ([`model_cache_dir`]); later runs load from the cache offline.

use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config, DTYPE};
use snafu::{OptionExt, ResultExt};
use tokenizers::{PaddingParams, Tokenizer, TruncationParams};

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

/// The backend-major token carried in `model_id()` — the candle pin and
/// [`MODEL_REVISION`] move together with it. Limitation inherited from the
/// fastembed backend: numeric drift within the same major is reported
/// identically (mismatch is detected only at this granularity).
const CANDLE_BACKEND_VERSION: &str = "1";

/// sentence-transformers' configured `max_seq_length` for this model
/// (`sentence_bert_config.json`); texts are truncated to it.
const MAX_SEQ_TOKENS: usize = 256;

/// Texts per forward pass. Bounds peak memory; MiniLM-L6 is small enough
/// that 32 keeps the CPU busy without ballooning activations.
const EMBED_BATCH: usize = 32;

/// all-MiniLM-L6-v2's vector width (the BERT `hidden_size`).
const MODEL_DIMENSIONS: usize = 384;

/// Maps candle/hf-hub/tokenizer failures into the crate error.
fn embedding_failed(source: impl std::fmt::Display) -> crate::error::Error {
    EmbeddingSnafu {
        reason: source.to_string(),
    }
    .build()
}

/// The stable identity of the default model —
/// `candle/<repo>@candle-<backend-major>` (e.g. `...@candle-1`), derived from
/// static metadata only: read-only callers like the CLI's `index status` can
/// compare a store's recorded identity against the current default without
/// loading (or downloading) the model.
fn model_id() -> String {
    format!("candle/{MODEL_REPO}@candle-{CANDLE_BACKEND_VERSION}")
}

/// The tokenizer plus the loaded BERT encoder, bundled so the provider can
/// own them under one mutex-guarded slot.
struct Model {
    tokenizer: Tokenizer,
    encoder: BertModel,
    device: Device,
}

impl Model {
    /// Downloads any missing weights into `cache` (module docs), then builds
    /// the tokenizer and the encoder from the cached files.
    fn load(cache: &std::path::Path) -> Result<Self> {
        let api =
            hf_hub::api::sync::ApiBuilder::from_cache(hf_hub::Cache::new(cache.to_path_buf()))
                .build()
                .map_err(embedding_failed)?;
        let repo = api.repo(hf_hub::Repo::with_revision(
            MODEL_REPO.to_string(),
            hf_hub::RepoType::Model,
            MODEL_REVISION.to_string(),
        ));
        // Cache-first lookups: every file resolves offline once downloaded.
        let weights_path = repo.get("model.safetensors").map_err(embedding_failed)?;
        let config_path = repo.get("config.json").map_err(embedding_failed)?;
        let tokenizer_path = repo.get("tokenizer.json").map_err(embedding_failed)?;

        let config: Config = serde_json::from_str(
            &fs::read_to_string(&config_path).context(IoSnafu { path: config_path })?,
        )
        .map_err(embedding_failed)?;
        let mut tokenizer = Tokenizer::from_file(tokenizer_path).map_err(embedding_failed)?;
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: MAX_SEQ_TOKENS,
                ..Default::default()
            }))
            .map_err(embedding_failed)?;
        tokenizer.with_padding(Some(PaddingParams::default()));

        // Buffered (not mmap'd) loading keeps the build free of `unsafe`; the
        // ~90 MB copy is transient and freed once the weights are materialized.
        let weights = fs::read(&weights_path).context(IoSnafu { path: weights_path })?;
        let device = Device::Cpu;
        let vb = VarBuilder::from_buffered_safetensors(weights, DTYPE, &device)
            .map_err(embedding_failed)?;
        let encoder = BertModel::load(vb, &config).map_err(embedding_failed)?;
        Ok(Self {
            tokenizer,
            encoder,
            device,
        })
    }

    /// Embeds one batch: tokenize (truncate to [`MAX_SEQ_TOKENS`], pad to the
    /// batch longest), run the encoder, mean-pool over unmasked token
    /// positions, L2-normalize.
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let encodings = self
            .tokenizer
            .encode_batch(texts.iter().map(String::as_str).collect::<Vec<_>>(), true)
            .map_err(embedding_failed)?;
        let attention_mask = Tensor::new(
            encodings
                .iter()
                .map(|e| {
                    e.get_attention_mask()
                        .iter()
                        .map(|&m| m as f32)
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>(),
            &self.device,
        )
        .map_err(embedding_failed)?;
        let input_ids = Tensor::new(
            encodings
                .iter()
                .map(|e| e.get_ids().to_vec())
                .collect::<Vec<_>>(),
            &self.device,
        )
        .map_err(embedding_failed)?;
        let token_type_ids = Tensor::new(
            encodings
                .iter()
                .map(|e| e.get_type_ids().to_vec())
                .collect::<Vec<_>>(),
            &self.device,
        )
        .map_err(embedding_failed)?;

        // `BertModel::forward` turns the 0/1 mask into the additive attention
        // mask internally; the raw mask is reused here for the pooling sum.
        let hidden = self
            .encoder
            .forward(&input_ids, &token_type_ids, Some(&attention_mask))
            .map_err(embedding_failed)?;
        let pooled = mean_pool_normalize(&hidden, &attention_mask).map_err(embedding_failed)?;
        pooled.to_vec2::<f32>().map_err(embedding_failed)
    }
}

/// Sentence-transformers pooling: the attention-mask-weighted mean of the
/// token positions, followed by L2 normalization — the exact post-processing
/// the `all-MiniLM-L6-v2` pipeline applies to the encoder output. `hidden`
/// is `(batch, seq, hidden)`, `attention_mask` `(batch, seq)` of 0/1; the
/// result is `(batch, hidden)`.
fn mean_pool_normalize(hidden: &Tensor, attention_mask: &Tensor) -> candle_core::Result<Tensor> {
    let mask = attention_mask.to_dtype(DType::F32)?.unsqueeze(2)?; // (b, s, 1)
    let summed = hidden.broadcast_mul(&mask)?.sum(1)?; // (b, d)
    let counts = mask.sum(1)?; // (b, 1); >= 1 because [CLS]/[SEP] are unmasked
    let mean = summed.broadcast_div(&counts)?;
    let norm = mean.sqr()?.sum_keepdim(1)?.sqrt()?; // (b, 1)
    mean.broadcast_div(&norm)
}

/// A local candle [`EmbeddingProvider`]: BERT text embeddings with no remote
/// service and no C dependency (see module docs for the first-run-download
/// tolerance).
pub struct CandleProvider {
    model: Mutex<Model>,
    model_id: String,
    dimensions: usize,
}

impl CandleProvider {
    /// The vector width every `embed` call produces (all-MiniLM-L6-v2).
    pub const DEFAULT_DIMENSIONS: usize = MODEL_DIMENSIONS;

    /// The model identity [`CandleProvider::new_default`] will report,
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

/// A [`CandleProvider`] that defers model construction — and the first-run
/// ~90 MB download — to the first [`EmbeddingProvider::embed`] call. Identity
/// and dimensionality derive from static metadata, so hash-diff previews
/// never load the model and a serving process starts instantly;
/// embedding-dependent ops fail with an actionable hint.
pub struct LazyCandleProvider {
    model_id: String,
    dimensions: usize,
    model: Mutex<Option<CandleProvider>>,
}

impl LazyCandleProvider {
    /// A lazy provider over the pinned default model. Never downloads or
    /// constructs anything until the first `embed`.
    pub fn new_default() -> Result<Self> {
        Ok(Self {
            model_id: CandleProvider::default_model_id(),
            dimensions: CandleProvider::DEFAULT_DIMENSIONS,
            model: Mutex::new(None),
        })
    }
}

impl EmbeddingProvider for LazyCandleProvider {
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
            let provider = CandleProvider::new_default().map_err(|source| {
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

impl EmbeddingProvider for CandleProvider {
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
        let device = Device::Cpu;
        // Two sequences of three positions over a 2-dim hidden state.
        let hidden = Tensor::new(
            vec![
                vec![1.0f32, 1.0, 2.0, 2.0, 3.0, 3.0],
                vec![-1.0, 1.0, 0.0, 0.0, 1.0, 1.0],
            ],
            &device,
        )
        .unwrap()
        .reshape((2, 3, 2))
        .unwrap();
        // The second position of the first sequence is padding.
        let mask = Tensor::new(
            vec![vec![1.0f32, 1.0, 0.0], vec![1.0f32, 1.0, 1.0]],
            &device,
        )
        .unwrap();
        let out = mean_pool_normalize(&hidden, &mask)
            .unwrap()
            .to_vec2::<f32>()
            .unwrap();
        // Sequence 0: masked mean [1.5, 1.5], L2-normalized to 1/sqrt(2).
        let unit = 1.0 / 2.0f32.sqrt();
        assert!((out[0][0] - unit).abs() < 1e-6 && (out[0][1] - unit).abs() < 1e-6);
        // Sequence 1: mean [0, 2/3], L2-normalized to [0, 1].
        assert!(out[1][0].abs() < 1e-6 && (out[1][1] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn model_identity_is_offline_derivable_and_stable() {
        assert_eq!(CandleProvider::default_model_id(), model_id());
        let id = model_id();
        assert!(
            id.starts_with("candle/") && id.contains("all-MiniLM-L6-v2") && id.contains("@candle-"),
            "model_id() follows candle/<model>@candle-<major>: {id}"
        );
    }

    /// The single candle test: needs network on a cold model cache, so it
    /// never runs in default `cargo test`.
    #[test]
    #[ignore = "downloads the model weights; run with --ignored"]
    fn default_model_embeds_384_dims_and_reports_a_stable_identity() {
        let a = CandleProvider::new_default().unwrap();
        let b = CandleProvider::new_default().unwrap();
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
