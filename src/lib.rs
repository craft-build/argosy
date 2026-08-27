//! Argosy: libraries for creating, consuming, and using OKF knowledge bundles.
//!
//! This layer provides the foundation every later module builds on: the
//! crate-wide [`Error`] type and [`Concept`], the markdown-plus-frontmatter
//! document all parsing, validation, and indexing operate on.

pub mod concept;
pub mod error;

pub use concept::{Concept, ConceptId};
pub use error::{Error, Result};
