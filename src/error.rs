//! Crate-wide error type.

use std::path::PathBuf;

use snafu::Snafu;

use crate::concept::ConceptId;

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

    /// A write or promotion targeted a concept id that already exists; the
    /// library never overwrites silently except through the deliberate
    /// `write_concept` edit path.
    #[snafu(display("concept `{id}` already exists"))]
    ConceptExists { id: ConceptId },

    /// A read, delete, or promotion source named a concept id with no file.
    #[snafu(display("concept `{id}` does not exist"))]
    ConceptNotFound { id: ConceptId },

    /// A write targeted a filename reserved by the bundle format (§4.4:
    /// `argosy.md`, `index.md`, `log.md`).
    #[snafu(display(
        "`argosy.md`, `index.md`, and `log.md` are reserved filenames and cannot be write targets"
    ))]
    ReservedFilename,

    /// A write violated the target namespace's hard contract (e.g. `STG-2`
    /// when a `styleguide/` concept lacks `type: Styleguide Rule`).
    #[snafu(display("namespace contract violation ({requirement}): {detail}"))]
    NamespaceContractViolation {
        requirement: &'static str,
        detail: String,
    },

    /// A path could not be opened as an argosy bundle root (`Argosy::open`'s
    /// hard failures: not a directory, or no parseable `Argosy Manifest`
    /// concept at `argosy.md`).
    #[snafu(display("`{}` is not an openable argosy bundle: {reason}", path.display()))]
    NotAnArgosy { path: PathBuf, reason: String },

    /// A project context activated two argosys with the same manifest name
    /// (`MUL-5`: identity by name must be unambiguous). The colliding name is
    /// included.
    #[snafu(display(
        "duplicate argosy name `{name}`: every active argosy must have a distinct manifest name (MUL-5)"
    ))]
    DuplicateArgosyName { name: String },

    /// A qualified lookup named an argosy that is not active in the project
    /// context (neither the local argosy nor any imported one).
    #[snafu(display("no active argosy named `{name}`"))]
    UnknownArgosy { name: String },

    /// An `argosy://` URI failed strict parsing (wrong scheme, missing
    /// segments, non-reserved namespace, or out-of-charset id).
    #[snafu(display("invalid argosy URI `{uri}`: {reason}"))]
    InvalidUri { uri: String, reason: String },

    /// An embedding provider or vector store could not carry out an index
    /// operation (embed, upsert, search, ...); the reason names the failing
    /// backend and operation.
    #[snafu(display("index operation failed: {reason}"))]
    Index { reason: String },

    /// The fastembed embedding provider failed to initialize the ONNX model
    /// (including the first-run download) or to embed a batch.
    #[cfg(feature = "default-index")]
    #[snafu(display("fastembed embedding failed: {source}"))]
    Embedding { source: fastembed::Error },

    /// A bundle file was a symlink whose resolved target escapes the bundle
    /// root. Packaging refuses to follow it (`DIST-3`'s same root-containment
    /// rule as the readers): materializing it would smuggle outside content
    /// into the distributed artifact.
    #[snafu(display(
        "symlink `{}` escapes the bundle root and cannot be packaged",
        path.display()
    ))]
    SymlinkEscape { path: PathBuf },

    /// Recomputing a bundle's `DIST-6` integrity sidecar disagreed with what
    /// is on disk (missing sidecar, missing file, or hash mismatch).
    #[snafu(display("integrity check failed for `{}`: {reason}", path.display()))]
    IntegrityMismatch { path: PathBuf, reason: String },

    /// The sqlite-vec index store failed on a database operation.
    #[cfg(feature = "default-index")]
    #[snafu(display("SQLite index operation failed: {source}"))]
    Sqlite { source: rusqlite::Error },
}

/// Convenient alias for `std::result::Result` with [`Error`].
pub type Result<T> = std::result::Result<T, Error>;
