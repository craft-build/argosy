//! Crate-wide error type.

use std::path::PathBuf;

use snafu::Snafu;

use crate::concept::ConceptId;

/// All errors argosy can produce.
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

    /// A required frontmatter field was absent.
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

    /// The user-level BarkML configuration could not be read or parsed.
    #[snafu(display("failed to load configuration: {source}"))]
    Config {
        #[snafu(source(from(barkml::Error, Box::new)))]
        source: Box<barkml::Error>,
    },

    /// Serving infrastructure failed (async runtime startup, transport handshake,
    /// serve loop).
    ///
    /// Distinct from validation so callers can tell "bad input" from "the server
    /// could not run".
    #[snafu(display("transport error: {reason}"))]
    Transport { reason: String },

    /// A write or promotion targeted a concept id that already exists.
    ///
    /// The library never overwrites silently except through the deliberate
    /// `write_concept` edit path.
    #[snafu(display("concept `{id}` already exists"))]
    ConceptExists { id: ConceptId },

    /// A read, delete, or promotion source named a concept id with no file.
    #[snafu(display("concept `{id}` does not exist"))]
    ConceptNotFound { id: ConceptId },

    /// A write targeted a filename reserved by the bundle format.
    #[snafu(display(
        "`argosy.md`, `index.md`, and `log.md` are reserved filenames and cannot be write targets"
    ))]
    ReservedFilename,

    /// A write violated the target namespace's hard contract.
    #[snafu(display("namespace contract violation ({requirement}): {detail}"))]
    NamespaceContractViolation {
        requirement: &'static str,
        detail: String,
    },

    /// A path could not be opened as an argosy bundle root (not a
    /// directory, or no parseable `Argosy Manifest` at `argosy.md`).
    #[snafu(display("`{}` is not an openable argosy bundle: {reason}", path.display()))]
    NotAnArgosy { path: PathBuf, reason: String },

    /// A project context activated two argosys with the same manifest name.
    #[snafu(display(
        "duplicate argosy name `{name}`: every active argosy must have a distinct manifest name (MUL-5)"
    ))]
    DuplicateArgosyName { name: String },

    /// A qualified lookup named an argosy that is not active in the project context.
    #[snafu(display("no active argosy named `{name}`"))]
    UnknownArgosy { name: String },

    /// An `argosy://` URI failed strict parsing.
    #[snafu(display("invalid argosy URI `{uri}`: {reason}"))]
    InvalidUri { uri: String, reason: String },

    /// An embedding provider or vector store could not carry out an index
    /// operation (embed, upsert, search, ...).
    #[snafu(display("index operation failed: {reason}"))]
    Index { reason: String },

    /// The default embedding provider failed to initialize the local model
    /// (including the first-run download) or to embed a batch.
    #[cfg(feature = "default-index")]
    #[snafu(display("embedding model failed: {reason}"))]
    Embedding { reason: String },

    /// A Jev-compatible decision-provider call failed: no endpoint is
    /// enabled, the endpoint is unreachable, or it returned an unexpected
    /// response.
    #[snafu(display("decision provider failed: {reason}"))]
    Decision { reason: String },

    /// A bundle file was a symlink whose resolved target escapes the bundle
    /// root.
    ///
    /// Packaging refuses to follow it: materializing it would smuggle
    /// outside content into the distributed artifact.
    #[snafu(display(
        "symlink `{}` escapes the bundle root and cannot be packaged",
        path.display()
    ))]
    SymlinkEscape { path: PathBuf },

    /// Recomputing a bundle's integrity sidecar disagreed with what is on disk.
    #[snafu(display("integrity check failed for `{}`: {reason}", path.display()))]
    IntegrityMismatch { path: PathBuf, reason: String },

    /// The sqlite-vec index store failed on a database operation.
    #[cfg(feature = "default-index")]
    #[snafu(display("SQLite index operation failed at `{}`: {source}", path.display()))]
    Sqlite {
        path: PathBuf,
        source: rusqlite::Error,
    },

    /// Starting a git command for a review failed.
    #[cfg(feature = "code-tools")]
    #[snafu(display("review operation failed while {operation}: {source}"))]
    ReviewIo {
        operation: String,
        source: std::io::Error,
    },

    /// Git produced a diff that cannot be represented as UTF-8.
    #[cfg(feature = "code-tools")]
    #[snafu(display("{artifact} was not valid UTF-8: {source}"))]
    ReviewEncoding {
        artifact: &'static str,
        source: std::string::FromUtf8Error,
    },

    /// A code-intelligence tool failed on caller input.
    #[cfg(feature = "code-tools")]
    #[snafu(display("{message}"))]
    CodeTool { message: String },
}

/// Convenient alias for `std::result::Result` with [`Error`].
pub type Result<T> = std::result::Result<T, Error>;
