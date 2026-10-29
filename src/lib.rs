//! Argosy: libraries for creating, consuming, and using OKF knowledge bundles.
//!
//! The foundation: the crate-wide [`Error`] type, the [`Concept`] document
//! everything operates on, and [`Argosy`], which opens and validates a bundle.

pub mod bundle;
pub mod catalog;
#[cfg(feature = "code-tools")]
pub mod codetools;
pub mod concept;
pub mod config;
pub mod context;
#[cfg(feature = "decision")]
pub mod decision;
pub mod error;
pub mod harness;
mod hash;
pub mod index;
pub mod local;
pub mod lockfile;
#[cfg(feature = "mcp")]
pub mod mcp;
pub mod package;
pub mod pull;
pub mod skill;
pub mod styleguide;
#[cfg(test)]
pub(crate) mod testutil;
pub mod trust;

pub use bundle::{Argosy, Finding, Manifest, Namespace, Severity, ValidationReport};
pub use concept::{Concept, ConceptId};
pub use config::Config;
pub use error::{Error, Result};
pub use harness::{Harness, ReviewerSetup, setup_reviewer};
pub use local::{LocalArgosy, Promotion, PromotionTarget};
pub use package::{ImportReport, PackageFormat, PackageOptions, PackageReport};
pub use skill::{Skill, SkillForm};
pub use styleguide::StyleguideRule;
pub use trust::{TrustPolicy, TrustTier};
