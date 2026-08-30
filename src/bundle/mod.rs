//! Opening and structural validation of an argosy bundle.
//!
//! [`Argosy::open`] errors only on hard failures (no readable root, no
//! parseable `Argosy Manifest`). [`Argosy::validate`] reports every issue
//! as [`Finding`]s instead of rejecting over tolerable ones.

mod argosy;
mod manifest;
mod namespace;
mod validation;
mod walk;

#[cfg(test)]
mod tests;

pub use argosy::Argosy;
pub use manifest::Manifest;
pub(crate) use manifest::is_safe_bundle_name;
pub use namespace::Namespace;
pub use validation::{Finding, Severity, ValidationReport};

pub(crate) use walk::sorted_walk;
