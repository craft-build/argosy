//! Crate-wide error type.

use std::path::PathBuf;

use snafu::Snafu;

/// All errors argosy can produce. Marked `#[non_exhaustive]`: later layers
/// (bundles, indexing, packaging) add variants without breaking matches.
#[non_exhaustive]
#[derive(Debug, Snafu)]
#[snafu(visibility(pub(crate)))]
pub enum Error {
    /// An I/O operation failed on a specific file.
    #[snafu(display("I/O error on `{}`: {source}", path.display()))]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },

    /// YAML frontmatter failed to parse in a specific file.
    #[snafu(display(
        "failed to parse YAML frontmatter in `{}`: {source}",
        path.display()
    ))]
    Yaml {
        path: PathBuf,
        source: yaml_serde::Error,
    },

    /// A `---` opener had no closing delimiter, or the block was not a YAML mapping.
    #[snafu(display(
        "missing or malformed frontmatter{}",
        path.as_ref().map(|p| format!(" in `{}`", p.display())).unwrap_or_default()
    ))]
    MissingFrontmatter { path: Option<PathBuf> },

    /// A required frontmatter field was absent. The field name is included.
    #[snafu(display(
        "missing required field `{field}`{}",
        path.as_ref().map(|p| format!(" in `{}`", p.display())).unwrap_or_default()
    ))]
    MissingField {
        field: &'static str,
        path: Option<PathBuf>,
    },

    /// Catch-all validation failure with a human-readable message.
    #[snafu(display("{reason}"))]
    Validation { reason: String },

    /// A path could not be opened as an argosy bundle root (`Argosy::open`'s
    /// hard failures: not a directory, or no parseable `Argosy Manifest`
    /// concept at `argosy.md`).
    #[snafu(display("`{}` is not an openable argosy bundle: {reason}", path.display()))]
    NotAnArgosy { path: PathBuf, reason: String },
}

/// Convenient alias for `std::result::Result` with [`Error`].
pub type Result<T> = std::result::Result<T, Error>;
