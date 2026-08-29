//! Argosy: libraries for creating, consuming, and using OKF knowledge bundles.
//! The foundation every later module builds on: the crate-wide [`Error`]
//! type, [`Concept`] — the markdown-plus-frontmatter document all parsing,
//! validation, and indexing operate on — and [`Argosy`], which opens and
//! structurally validates a bundle on disk.

pub mod bundle;
pub mod concept;
pub mod context;
pub mod error;
mod hash;
pub mod index;
pub mod local;
#[cfg(feature = "mcp")]
pub mod mcp;
pub mod package;
pub mod pull;
pub mod skill;
pub mod styleguide;
#[cfg(test)]
pub(crate) mod testutil;

pub use bundle::{Argosy, Finding, Manifest, Namespace, Severity, ValidationReport};
pub use concept::{Concept, ConceptId};
pub use error::{Error, Result};
pub use local::{LocalArgosy, Promotion, PromotionTarget};
pub use package::{ImportReport, PackageFormat, PackageOptions, PackageReport};
pub use skill::{Skill, SkillForm};
pub use styleguide::StyleguideRule;
