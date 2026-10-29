//! Opening and structural validation of an argosy bundle.
//!
//! [`Argosy::open`] errors only on hard failures; [`Argosy::validate`]
//! reports every issue as a [`Finding`] instead of rejecting.

mod argosy;
mod manifest;
mod namespace;
mod validation;
mod walk;

#[cfg(test)]
mod tests;

pub use argosy::Argosy;
pub use manifest::Manifest;
pub(crate) use manifest::{is_real_dir, is_real_file, is_safe_bundle_name};
pub use namespace::Namespace;
pub use validation::{Finding, Severity, ValidationReport};

pub(crate) use walk::sorted_walk;
