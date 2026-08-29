//! The fastembed-backed default [`EmbeddingProvider`], gated behind the
//! `default-index` Cargo feature. fastembed runs ONNX locally — no live
//! network needed except the first construction, which downloads the model
//! weights (~90 MB) into fastembed's cache (`$FASTEMBED_CACHE` or the
//! platform cache dir); later runs load from the cache offline.

use std::sync::Mutex;

use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};
use snafu::ResultExt;

use crate::error::{EmbeddingSnafu, IndexSnafu, Result};

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
    /// the model on first use (module docs).
    pub fn with_model(model: EmbeddingModel) -> Result<Self> {
        let model_id = Self::model_id_for(&model)?;
        let dimensions = TextEmbedding::get_model_info(&model)
            .context(EmbeddingSnafu)?
            .dim;
        let model = TextEmbedding::try_new(TextInitOptions::new(model)).context(EmbeddingSnafu)?;
        Ok(Self {
            model: Mutex::new(model),
            model_id,
            dimensions,
        })
    }
}

/// A [`FastembedProvider`] that defers model construction — and the
/// first-run ~90 MB download — to the first [`EmbeddingProvider::embed`]
/// call. Identity and dimensionality derive from static metadata, so
/// hash-diff reconcile and `index status`-style previews never load the
/// model: a serving process (the MCP server) starts instantly and works
/// fully offline except for embedding-dependent operations, which fail
/// with an actionable hint instead of taking the whole server down.
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
