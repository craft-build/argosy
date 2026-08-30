//! Unit tests for the sqlite-vec store.

mod lifecycle;
mod search;
mod store;

use tempfile::TempDir;

use super::*;
use crate::index::tests::{MockEmbedder, fixture};
use crate::index::{EmbeddingProvider, EmbeddingUnit, Index, Query, UnitMeta, VectorStore};

fn open_in_tmp() -> (TempDir, SqliteVecStore) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join(".argosy/index.db");
    let store = SqliteVecStore::open(&path).unwrap();
    (dir, store)
}

/// Embeds `text` and wraps it into a unit with the given identity/facets.
fn make_unit(
    embedder: &MockEmbedder,
    argosy: &str,
    namespace: Namespace,
    id: &str,
    text: &str,
    meta: UnitMeta,
) -> EmbeddingUnit {
    EmbeddingUnit {
        concept: QualifiedConceptId {
            argosy: argosy.to_string(),
            namespace,
            id: id.parse().unwrap(),
        },
        chunk_ordinal: 0,
        text_hash: text.len().to_string(),
        vector: embedder.embed(&[text.to_string()]).unwrap()[0].clone(),
        meta,
    }
}

fn meta(
    concept_type: Option<&str>,
    tags: &[&str],
    language: Option<&str>,
    category: Option<&str>,
) -> UnitMeta {
    UnitMeta {
        concept_type: concept_type.map(str::to_string),
        description: None,
        tags: tags.iter().map(|s| s.to_string()).collect(),
        language: language.map(str::to_string),
        category: category.map(str::to_string),
    }
}

/// The standard corpus: three concepts across two argosies/namespaces
/// with distinct facets, so every Filter dimension has a discriminant.
fn seed(store: &mut SqliteVecStore, embedder: &MockEmbedder) -> Vec<EmbeddingUnit> {
    let units = vec![
        make_unit(
            embedder,
            "local",
            Namespace::Document,
            "document/arch",
            "water flows downhill through valleys",
            meta(Some("Note"), &["geo", "rust"], None, None),
        ),
        make_unit(
            embedder,
            "local",
            Namespace::Skill,
            "skill/deploy",
            "rust compile cargo build release",
            meta(Some("Skill"), &["rust"], None, None),
        ),
        make_unit(
            embedder,
            "vendor",
            Namespace::Styleguide,
            "styleguide/rust/naming",
            "naming conventions snake case identifiers",
            meta(
                Some("Styleguide Rule"),
                &["style"],
                Some("rust"),
                Some("naming"),
            ),
        ),
    ];
    store.upsert(&units).unwrap();
    units
}
