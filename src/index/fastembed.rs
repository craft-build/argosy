//! The fastembed-backed default [`EmbeddingProvider`], gated behind the
//! `default-index` Cargo feature. fastembed runs ONNX locally — no live
//! network needed except the first construction, which downloads the model
//! weights (~90 MB) into a user-level cache ([`model_cache_dir`]:
//! `$FASTEMBED_CACHE_DIR` or the XDG cache dir); later runs load from the
//! cache offline.

use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};
use snafu::{OptionExt, ResultExt};

use crate::error::{EmbeddingSnafu, IndexSnafu, IoSnafu, Result};

use super::EmbeddingProvider;

/// The version token carried in `model_id()`. Mirrors the `fastembed = "6"`
/// pin in `Cargo.toml` — bump them together. Limitation: weights re-published
/// within the `6.x` line are reported identically (mismatch is detected only
/// at backend-major granularity).
const FASTEMBED_BACKEND_VERSION: &str = "6";

/// A local fastembed [`EmbeddingProvider`]: ONNX text embeddings with no
/// remote service (see module docs for the first-run-download tolerance).
///
/// The default model is `all-MiniLM-L6-v2` (384-dim) — small enough to keep
/// indexing interactive on the corpora argosy targets.
pub struct FastembedProvider {
    // `Mutex` makes the provider usable through `&self` regardless of
    // whether fastembed's `embed` wants `&self` or `&mut self`.
    model: Mutex<TextEmbedding>,
    model_id: String,
    dimensions: usize,
}

impl FastembedProvider {
    /// The prescribed default model (`all-MiniLM-L6-v2`, 384-dim).
    pub const DEFAULT_MODEL: EmbeddingModel = EmbeddingModel::AllMiniLML6V2;

    /// Creates a provider over [`Self::DEFAULT_MODEL`]. Downloads the model
    /// on first use (module docs).
    pub fn new_default() -> Result<Self> {
        Self::with_model(Self::DEFAULT_MODEL)
    }

    /// The model identity [`FastembedProvider::new_default`] will report,
    /// without constructing (or downloading) the model — the model identity
    /// derives from static metadata (`TextEmbedding::get_model_info`), so
    /// read-only callers like the CLI's `index status` can compare a store's
    /// recorded identity against the current default offline.
    pub fn default_model_id() -> Result<String> {
        Self::model_id_for(&Self::DEFAULT_MODEL)
    }

    /// Stable across runs (derived from the model's static metadata)
    /// and changes when the model changes (the model code identifies the
    /// weights, the suffix the backend's major).
    fn model_id_for(model: &EmbeddingModel) -> Result<String> {
        let info = TextEmbedding::get_model_info(model).context(EmbeddingSnafu)?;
        Ok(format!(
            "fastembed/{}@fastembed-{FASTEMBED_BACKEND_VERSION}",
            info.model_code
        ))
    }

    /// Creates a provider over any fastembed [`EmbeddingModel`]. Downloads
    /// the model on first use into [`model_cache_dir`] (module docs).
    pub fn with_model(model: EmbeddingModel) -> Result<Self> {
        let model_id = Self::model_id_for(&model)?;
        let dimensions = TextEmbedding::get_model_info(&model)
            .context(EmbeddingSnafu)?
            .dim;
        let cache = model_cache_dir()?;
        // Create before fastembed touches it: a clear, early error for an
        // unwritable cache location instead of a mid-download failure.
        fs::create_dir_all(&cache).context(IoSnafu {
            path: cache.clone(),
        })?;
        let model = TextEmbedding::try_new(TextInitOptions::new(model).with_cache_dir(cache))
            .context(EmbeddingSnafu)?;
        Ok(Self {
            model: Mutex::new(model),
            model_id,
            dimensions,
        })
    }
}

/// Where the ONNX model weights are cached. fastembed's default is a
/// cwd-relative `.fastembed_cache/` — a fresh copy per project; argosy
/// instead uses one shared user-level cache: `$FASTEMBED_CACHE_DIR` when
/// set, else `$XDG_CACHE_HOME/argosy/fastembed` (falling back to
/// `~/.cache/argosy/fastembed`). `$HF_HOME` overrides inside fastembed.
pub fn model_cache_dir() -> Result<PathBuf> {
    cache_dir_from(
        std::env::var_os("FASTEMBED_CACHE_DIR"),
        std::env::var_os("XDG_CACHE_HOME"),
        std::env::var_os("HOME"),
    )
}

/// Pure core of [`model_cache_dir`], env reads factored out for tests.
fn cache_dir_from(
    fastembed_cache: Option<OsString>,
    xdg_cache_home: Option<OsString>,
    home: Option<OsString>,
) -> Result<PathBuf> {
    if let Some(dir) = fastembed_cache
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
    {
        return Ok(dir);
    }
    let base = xdg_cache_home
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| home.map(|home| PathBuf::from(home).join(".cache")))
        .context(IndexSnafu {
            reason: "cannot locate the embedding-model cache: set FASTEMBED_CACHE_DIR, \
                     XDG_CACHE_HOME, or HOME"
                .to_string(),
        })?;
    Ok(base.join("argosy").join("fastembed"))
}

/// A [`FastembedProvider`] that defers model construction — and the
/// first-run ~90 MB download — to the first [`EmbeddingProvider::embed`]
/// call. Identity and dimensionality derive from static metadata, so
/// hash-diff previews never load the model and a serving process starts
/// instantly; embedding-dependent ops fail with an actionable hint.
pub struct LazyFastembedProvider {
    model_id: String,
    dimensions: usize,
    model: Mutex<Option<FastembedProvider>>,
}

impl LazyFastembedProvider {
    /// A lazy provider over [`FastembedProvider::DEFAULT_MODEL`]. Never
    /// downloads or constructs anything until the first `embed`.
    pub fn new_default() -> Result<Self> {
        Ok(Self {
            model_id: FastembedProvider::default_model_id()?,
            dimensions: TextEmbedding::get_model_info(&FastembedProvider::DEFAULT_MODEL)
                .context(EmbeddingSnafu)?
                .dim,
            model: Mutex::new(None),
        })
    }
}

impl EmbeddingProvider for LazyFastembedProvider {
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
                reason: "fastembed model mutex poisoned by a panicking caller".to_string(),
            }
            .build()
        })?;
        if slot.is_none() {
            let provider = FastembedProvider::new_default().map_err(|source| {
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

impl EmbeddingProvider for FastembedProvider {
    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let mut model = self.model.lock().map_err(|_| -> crate::error::Error {
            IndexSnafu {
                reason: "fastembed model mutex poisoned by a panicking caller".to_string(),
            }
            .build()
        })?;
        model
            .embed(
                texts.iter().map(String::as_str).collect::<Vec<&str>>(),
                None,
            )
            .context(EmbeddingSnafu)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_dir_precedence_and_fallbacks() {
        // An explicit `$FASTEMBED_CACHE_DIR` wins and is honored verbatim.
        assert_eq!(
            cache_dir_from(
                Some("/fe".into()),
                Some("/xdg".into()),
                Some("/home".into())
            )
            .unwrap(),
            PathBuf::from("/fe")
        );
        // XDG next, namespaced under argosy/fastembed.
        assert_eq!(
            cache_dir_from(None, Some("/xdg".into()), Some("/home".into())).unwrap(),
            PathBuf::from("/xdg/argosy/fastembed")
        );
        // HOME fallback when XDG_CACHE_HOME is unset.
        assert_eq!(
            cache_dir_from(None, None, Some("/home".into())).unwrap(),
            PathBuf::from("/home/.cache/argosy/fastembed")
        );
        // Empty strings count as unset (mirrors `global_argosy_dir`).
        assert_eq!(
            cache_dir_from(None, Some("".into()), Some("/home".into())).unwrap(),
            PathBuf::from("/home/.cache/argosy/fastembed")
        );
        assert_eq!(
            cache_dir_from(Some("".into()), None, Some("/home".into())).unwrap(),
            PathBuf::from("/home/.cache/argosy/fastembed")
        );
        // Nothing to derive from: an actionable error, never a CWD-relative
        // `.fastembed_cache/`.
        assert!(cache_dir_from(None, None, None).is_err());
    }

    /// The single fastembed test: needs network on a cold model cache, so it
    /// never runs in default `cargo test`.
    #[test]
    #[ignore = "downloads ONNX model; run with --ignored"]
    fn default_model_embeds_384_dims_and_reports_a_stable_identity() {
        let a = FastembedProvider::new_default().unwrap();
        let b = FastembedProvider::new_default().unwrap();
        assert_eq!(
            a.model_id(),
            b.model_id(),
            "IDX-5: two instances of the same model report identical ids"
        );
        assert!(
            a.model_id().starts_with("fastembed/")
                && a.model_id().contains("all-MiniLM-L6-v2")
                && a.model_id().contains("@fastembed-"),
            "model_id() follows fastembed/<model>@<version>: {}",
            a.model_id()
        );
        assert_eq!(a.dimensions(), 384);

        let vectors = a.embed(&["borrow checker basics".to_string()]).unwrap();
        assert_eq!(vectors.len(), 1);
        assert_eq!(vectors[0].len(), 384);

        // `with_model` over the same model agrees with `new_default`.
        assert_eq!(
            FastembedProvider::with_model(FastembedProvider::DEFAULT_MODEL)
                .unwrap()
                .model_id(),
            a.model_id()
        );
    }
}
