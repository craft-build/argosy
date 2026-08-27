//! Argosy: libraries for creating, consuming, and using OKF knowledge bundles.
//!
//! This layer provides the foundation every later module builds on: the
//! crate-wide [`Error`] type, [`Concept`] — the markdown-plus-frontmatter
//! document all parsing, validation, and indexing operate on — and [`Argosy`],
//! which opens and structurally validates a bundle on disk.

pub mod bundle;
pub mod concept;
pub mod error;

pub use bundle::{Argosy, Finding, Manifest, Namespace, Severity, ValidationReport};
pub use concept::{Concept, ConceptId};
pub use error::{Error, Result};
