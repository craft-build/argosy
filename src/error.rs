//! Crate-wide error type.

use std::path::PathBuf;

use snafu::Snafu;

use crate::concept::ConceptId;

/// All errors argosy can produce.
///
/// The `reason`/`description`-style String catch-alls are a deliberate
/// MCP-facing envelope: tool responses serialize one human-readable
/// message. Prefer typed variants whenever a caller could branch on the
/// cause.
#[non_exhaustive]
#[derive(Debug, Snafu)]
#[snafu(visibility(pub(crate)))]
pub enum Error {
    #[snafu(display("I/O error on `{}`: {source}", path.display()))]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },

    #[snafu(display(
        "failed to parse YAML frontmatter in `{}`: {source}",
        path.display()
    ))]
    Yaml {
        path: PathBuf,
        source: yaml_serde::Error,
    },

    #[snafu(display(
        "missing or malformed frontmatter{}",
        path.as_ref().map(|p| format!(" in `{}`", p.display())).unwrap_or_default()
    ))]
    MissingFrontmatter { path: Option<PathBuf> },

    #[snafu(display(
        "missing required field `{field}`{}",
        path.as_ref().map(|p| format!(" in `{}`", p.display())).unwrap_or_default()
    ))]
    MissingField {
        field: &'static str,
        path: Option<PathBuf>,
    },

    #[snafu(display("{reason}"))]
    Validation { reason: String },

    #[snafu(display("failed to load configuration: {source}"))]
    Config {
        #[snafu(source(from(barkml::Error, Box::new)))]
        source: Box<barkml::Error>,
    },

    // Distinct from validation so callers can tell "bad input" from "the
    // server could not run".
    #[snafu(display("transport error: {reason}"))]
    Transport { reason: String },

    // The library never overwrites silently except through the deliberate
    // `write_concept` edit path.
    #[snafu(display("concept `{id}` already exists"))]
    ConceptExists { id: ConceptId },

    #[snafu(display("concept `{id}` does not exist"))]
    ConceptNotFound { id: ConceptId },

    #[snafu(display(
        "`argosy.md`, `index.md`, and `log.md` are reserved filenames and cannot be write targets"
    ))]
    ReservedFilename,

    #[snafu(display("namespace contract violation ({requirement}): {detail}"))]
    NamespaceContractViolation {
        requirement: &'static str,
        detail: String,
    },

    #[snafu(display("`{}` is not an openable argosy bundle: {reason}", path.display()))]
    NotAnArgosy { path: PathBuf, reason: String },

    #[snafu(display(
        "duplicate argosy name `{name}`: every active argosy must have a distinct manifest name (MUL-5)"
    ))]
    DuplicateArgosyName { name: String },

    #[snafu(display("no active argosy named `{name}`"))]
    UnknownArgosy { name: String },

    #[snafu(display("invalid argosy URI `{uri}`: {reason}"))]
    InvalidUri { uri: String, reason: String },

    #[snafu(display("index operation failed: {reason}"))]
    Index { reason: String },

    #[cfg(feature = "default-index")]
    #[snafu(display("embedding model failed: {reason}"))]
    Embedding { reason: String },

    #[snafu(display("decision provider failed: {reason}"))]
    Decision { reason: String },

    // Packaging refuses to follow the symlink: materializing it would
    // smuggle outside content into the distributed artifact.
    #[snafu(display(
        "symlink `{}` escapes the bundle root and cannot be packaged",
        path.display()
    ))]
    SymlinkEscape { path: PathBuf },

    #[snafu(display("integrity check failed for `{}`: {reason}", path.display()))]
    IntegrityMismatch { path: PathBuf, reason: String },

    #[cfg(feature = "default-index")]
    #[snafu(display("SQLite index operation failed at `{}`: {source}", path.display()))]
    Sqlite {
        path: PathBuf,
        source: rusqlite::Error,
    },

    #[cfg(feature = "code-tools")]
    #[snafu(display("review operation failed while {operation}: {source}"))]
    ReviewIo {
        operation: String,
        source: std::io::Error,
    },

    #[cfg(feature = "code-tools")]
    #[snafu(display("{artifact} was not valid UTF-8: {source}"))]
    ReviewEncoding {
        artifact: &'static str,
        source: std::string::FromUtf8Error,
    },

    #[cfg(feature = "code-tools")]
    #[snafu(display("{message}"))]
    CodeTool { message: String },

    #[cfg(feature = "code-tools")]
    #[snafu(display("invalid glob pattern: {source}"))]
    GlobPattern { source: ignore::Error },

    #[snafu(display("failed to parse `{}`: {source}", path.display()))]
    LockfileParse {
        path: PathBuf,
        #[snafu(source(from(barkml::Error, Box::new)))]
        source: Box<barkml::Error>,
    },

    #[snafu(display("failed to read `{}` as a lockfile: {source}", path.display()))]
    LockfileFormat {
        path: PathBuf,
        #[snafu(source(from(barkml::Error, Box::new)))]
        source: Box<barkml::Error>,
    },

    #[cfg(feature = "decision")]
    #[snafu(display("failed to build the HTTP client: {source}"))]
    DecisionClientBuild { source: reqwest::Error },

    #[cfg(feature = "decision")]
    #[snafu(display("decision request to `{url}` failed: {source}"))]
    DecisionRequest { url: String, source: reqwest::Error },

    #[cfg(feature = "decision")]
    #[snafu(display("decision endpoint `{endpoint}` returned an unparseable response: {source}"))]
    DecisionResponseParse {
        endpoint: String,
        source: reqwest::Error,
    },

    #[cfg(feature = "mcp")]
    #[snafu(display("failed to start the tokio runtime: {source}"))]
    TransportRuntime { source: std::io::Error },

    #[cfg(feature = "mcp")]
    #[snafu(display("{operation}: {source}"))]
    TransportService {
        operation: &'static str,
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
}

/// Convenient alias for `std::result::Result` with [`Error`].
pub type Result<T> = std::result::Result<T, Error>;
