//! The reserved namespaces and their directory-name mapping.

use snafu::ensure;

use crate::error::{Result, ValidationSnafu};

use super::manifest::is_safe_component;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Namespace {
    /// The `document/` namespace: prose knowledge and decisions.
    Document,
    /// The `skill/` namespace: on-demand instructions.
    Skill,
    /// The `memory/` namespace: session-derived learnings.
    Memory,
    /// The `styleguide/` namespace: typed linting rules.
    Styleguide,
    /// Any other top-level directory; the name is preserved.
    Custom(String),
}

impl Namespace {
    /// The four reserved top-level namespace directory names.
    pub const RESERVED: [&'static str; 4] = ["document", "skill", "memory", "styleguide"];

    /// Filenames that are reserved everywhere in a bundle: the root
    /// manifest plus OKF's listing and change-history files.
    pub const RESERVED_FILENAMES: [&'static str; 3] = ["argosy.md", "index.md", "log.md"];

    /// The directory name this namespace occupies under the bundle root.
    pub fn as_dir_name(&self) -> &str {
        match self {
            Self::Document => "document",
            Self::Skill => "skill",
            Self::Memory => "memory",
            Self::Styleguide => "styleguide",
            Self::Custom(name) => name,
        }
    }

    /// True iff this is one of the four reserved namespaces.
    pub fn is_reserved(&self) -> bool {
        !matches!(self, Self::Custom(_))
    }

    /// Classifies a top-level directory name: reserved names map to their
    /// variants, anything else to [`Namespace::Custom`].
    /// Validity of the name itself (a directory called `index.md`, say) is a
    /// separate question answered by validation.
    pub fn from_dir_name(name: &str) -> Self {
        match name {
            "document" => Self::Document,
            "skill" => Self::Skill,
            "memory" => Self::Memory,
            "styleguide" => Self::Styleguide,
            other => Self::Custom(other.to_string()),
        }
    }

    /// Builds a [`Namespace::Custom`] after validating the name is a single
    /// safe path component — empty names, `.`/`..`, and separators are
    /// rejected, since anything else could traverse out of the bundle root
    /// when joined under it. (`from_dir_name` needs no check: it classifies
    /// names from `read_dir`, which are single components by construction.)
    pub fn custom(name: &str) -> Result<Self> {
        ensure!(
            is_safe_component(name),
            ValidationSnafu {
                reason: format!("invalid custom namespace name `{name}`")
            }
        );
        Ok(Self::Custom(name.to_string()))
    }

    /// `index.md` or `log.md` — OKF listing/history files that never count as
    /// concepts.
    pub(crate) fn is_listing_file(name: &str) -> bool {
        name == "index.md" || name == "log.md"
    }
}

impl serde::Serialize for Namespace {
    /// Serializes as the directory name (`document`, `custom-name`,...) so
    /// machine consumers see the spelling used on disk and in URIs.
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_dir_name())
    }
}
