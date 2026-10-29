//! User-level configuration, loaded from `~/.config/argosy` (BarkML).
//!
//! One user, one machine — nothing here is project specific. Either a
//! single `~/.config/argosy.bml` file or any number of `*.bml` files under
//! `~/.config/argosy/` (loaded in sorted file-name order); when both
//! exist, both load. Missing files are fine: the all-defaults [`Config`]
//! applies. Each key may be set in exactly one file — setting the same
//! key twice is reported as a configuration collision, never a silent
//! override.
//!
//! ```bml
//! output {
//!     quiet = false
//!     json = false
//! }
//! paths {
//!     embed_cache_dir = "~/.cache/argosy-embeddings"
//!     index_db_name = "index.db"
//! }
//! pull {
//!     default_global = false
//! }
//! index {
//!     model = "all-minilm-l6-v2"
//!     search_mode = "hybrid"  // or "lexical": BM25-only, zero model download
//!     default_k = 5
//!     mcp_default_k = 8
//!     chunk_max_tokens = 220
//! }
//! package {
//!     format = "dir"        // or "tar.gz"
//!     include_index = false
//! }
//! catalog {
//!     redact_home = false
//! }
//! code_tools {
//!     allow_external_paths = false
//! }
//! session {
//!     project_ttl_secs = 1800
//!     project_max = 8
//!     review_ttl_secs = 7200
//!     review_max = 16
//! }
//! trust {
//!     // Per-import maximum OKF tier, keyed by manifest name. An import
//!     // with no entry is capped at "unverified" whatever its concepts
//!     // claim in `verified` frontmatter.
//!     "acme-shared" = "machine-confirmed"
//! }
//! decision {
//!     enabled = false                    // Jev-compatible endpoint, off by default
//!     provider = "laya-serve"            // or "jev" for a hosted/other endpoint
//!     endpoint = "http://127.0.0.1:8000"
//!     api_key_env = "LAYA_API_KEY"       // name of an env var, never the secret
//!     timeout_ms = 3000
//!     model = "english"                  // optional passthrough
//!     max_input_tokens = 512             // trim over-budget requests (estimated)
//! }
//! ```
//!
//! Paths honor `$XDG_CONFIG_HOME` (falling back to `~/.config`). Location
//! of argosy state and cache data stays with the standard environment
//! (`$XDG_STATE_HOME`, `$ARGOSY_EMBED_CACHE_DIR`) except where this
//! config explicitly overrides it.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use barkml::{Loader, StandardLoader};
use serde::{Deserialize, Deserializer, Serialize};
use snafu::ResultExt;

/// Deserializes a `usize` from any BarkML integer. Plain literals lex as
/// signed (`5`), suffixed ones as unsigned (`5u64`); both spellings must
/// configure a count.
fn flex_uint<'de, D: Deserializer<'de>>(deserializer: D) -> std::result::Result<usize, D::Error> {
    struct V;
    impl serde::de::Visitor<'_> for V {
        type Value = usize;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a non-negative integer")
        }
        fn visit_i64<E: serde::de::Error>(self, n: i64) -> std::result::Result<usize, E> {
            usize::try_from(n).map_err(|_| E::custom("must be a non-negative integer"))
        }
        fn visit_u64<E: serde::de::Error>(self, n: u64) -> std::result::Result<usize, E> {
            usize::try_from(n).map_err(|_| E::custom("must be a non-negative integer"))
        }
    }
    deserializer.deserialize_any(V)
}

use crate::error::{ConfigSnafu, Result};
use crate::trust::{TrustPolicy, TrustTier};

/// The user-level configuration. Every field has a default matching the
/// pre-configuration behavior; command-line flags always win over it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Default output behavior of the CLI.
    pub output: OutputConfig,
    /// Filesystem locations argosy uses.
    pub paths: PathsConfig,
    /// Defaults for `argosy pull`.
    pub pull: PullConfig,
    /// Defaults for indexing and search.
    pub index: IndexConfig,
    /// Defaults for `argosy package`.
    pub package: PackageConfig,
    /// Defaults for `argosy catalog`.
    pub catalog: CatalogConfig,
    /// Defaults for the code-intelligence tools.
    pub code_tools: CodeToolsConfig,
    /// Session-cache bounds for MCP project and review sessions.
    pub session: SessionConfig,
    /// Per-import maximum OKF trust tiers (see [`TrustPolicy`]).
    pub trust: HashMap<String, TrustTier>,
    /// Defaults for the optional Jev-compatible decision endpoint.
    pub decision: DecisionConfig,
}

/// Output defaults for the CLI (`--quiet` / `--json`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OutputConfig {
    /// Suppress non-error human output by default.
    pub quiet: bool,
    /// Emit machine-readable JSON on stdout by default.
    pub json: bool,
}

/// Filesystem locations argosy uses.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PathsConfig {
    /// Where the embedding model weights are cached (default: the
    /// `~/.cache/argosy/embeddings` layout; same effect as setting
    /// `$ARGOSY_EMBED_CACHE_DIR`).
    pub embed_cache_dir: Option<PathBuf>,
    /// File name of the derived index inside a project's state directory.
    pub index_db_name: Option<String>,
}

/// Defaults for `argosy pull`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PullConfig {
    /// Install into the user-wide global store instead of the project's.
    pub default_global: bool,
}

/// Defaults for indexing and search.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IndexConfig {
    /// The embedding model for semantic indexing, by registry name
    /// (e.g. `"all-minilm-l6-v2"`). `None` is the compiled-in default.
    /// Changing it invalidates existing indexes — the next `index build`
    /// performs a full rebuild.
    pub model: Option<String>,
    /// How `search` ranks: BM25 fused with vector similarity (the
    /// default) or BM25 alone. See [`SearchModeConfig`].
    pub search_mode: SearchModeConfig,
    /// Default hit count for `argosy index query`.
    #[serde(deserialize_with = "flex_uint")]
    pub default_k: usize,
    /// Default hit count for the MCP `search`/`search_rules` tools.
    #[serde(deserialize_with = "flex_uint")]
    pub mcp_default_k: usize,
    /// Token budget for one embedded passage (prefix included). Long
    /// concepts split by heading into passages at or under this size;
    /// changing it re-chunks and re-embeds affected concepts on the next
    /// reconcile. Capped at the embedder's 256-token truncation.
    #[serde(deserialize_with = "flex_uint")]
    pub chunk_max_tokens: usize,
}

impl Default for IndexConfig {
    fn default() -> Self {
        Self {
            model: None,
            search_mode: SearchModeConfig::Hybrid,
            default_k: 5,
            mcp_default_k: 8,
            chunk_max_tokens: crate::index::chunk::DEFAULT_MAX_TOKENS,
        }
    }
}

/// The `index.search_mode` selector: how search ranks, and whether an
/// embedding model is involved at all.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SearchModeConfig {
    /// Hybrid retrieval: an FTS5 BM25 leg and a vector leg, fused by
    /// reciprocal rank fusion — exact identifiers rank next to semantic
    /// matches. The default.
    #[default]
    Hybrid,
    /// Lexical-only retrieval: BM25 over the same chunked passages, no
    /// embedding model is ever downloaded (the zero-model mode). Exact
    /// terms rank strongly; paraphrases do not. Switching between modes
    /// invalidates existing indexes — the next `index build` performs a
    /// full rebuild.
    #[serde(rename = "lexical")]
    Lexical,
}

/// Defaults for `argosy package`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PackageConfig {
    /// The default artifact format: `"dir"` or `"tar.gz"`.
    pub format: PackageFormatConfig,
    /// Ship the `.argosy/` index cache in artifacts by default.
    pub include_index: bool,
}

impl Default for PackageConfig {
    fn default() -> Self {
        Self {
            format: PackageFormatConfig::Dir,
            include_index: false,
        }
    }
}

/// Defaults for `argosy catalog`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CatalogConfig {
    /// Rewrite home-directory path prefixes as `~` in generated catalogs.
    pub redact_home: bool,
}

/// Defaults for the code-intelligence tools (`outline`, `zoom`, `astgrep`,
/// `conflicts`, `inspect`, `callgraph`, `repomap`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CodeToolsConfig {
    /// Let the code tools resolve absolute paths outside the workspace
    /// directory the server was spawned in. Off by default: every path
    /// parameter must stay inside the workspace, so a confused or
    /// injected caller cannot read — or, through `astgrep apply` /
    /// `conflicts resolve`, write — files outside it.
    pub allow_external_paths: bool,
}

/// Session-cache bounds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SessionConfig {
    /// Project-session idle TTL, in seconds.
    #[serde(deserialize_with = "flex_uint")]
    pub project_ttl_secs: usize,
    /// Maximum cached project sessions.
    #[serde(deserialize_with = "flex_uint")]
    pub project_max: usize,
    /// Review-session idle TTL, in seconds.
    #[serde(deserialize_with = "flex_uint")]
    pub review_ttl_secs: usize,
    /// Maximum review sessions.
    #[serde(deserialize_with = "flex_uint")]
    pub review_max: usize,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            project_ttl_secs: 1800,
            project_max: 8,
            review_ttl_secs: 7200,
            review_max: 16,
        }
    }
}

impl SessionConfig {
    /// The project-session idle TTL.
    pub fn project_ttl(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.project_ttl_secs as u64)
    }

    /// The review-session idle TTL.
    pub fn review_ttl(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.review_ttl_secs as u64)
    }
}

/// Defaults for the optional Jev-compatible decision endpoint (Laya's
/// `laya-serve`, TypeSafe Jev, or any clone). Off by default; when disabled,
/// every decision-backed feature degrades to its non-decision behavior.
///
/// The wire protocol is the same Jev `POST /v1/systemone` for every provider;
/// `provider` selects defaults only (default port, API-key env var name), not
/// behavior.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DecisionConfig {
    /// Enable calls to a Jev-compatible decision endpoint.
    pub enabled: bool,
    /// Which endpoint family this configuration targets.
    pub provider: DecisionProviderConfig,
    /// Base URL of the endpoint, e.g. `http://127.0.0.1:8000`. `None` uses the
    /// provider's default (only `laya-serve` has one).
    pub endpoint: Option<String>,
    /// Name of the environment variable holding the bearer token. The secret
    /// itself is never stored in configuration.
    pub api_key_env: Option<String>,
    /// Per-request timeout in milliseconds.
    #[serde(deserialize_with = "flex_uint")]
    pub timeout_ms: usize,
    /// Optional checkpoint/model passthrough honored by servers that support
    /// it (e.g. Laya's `english` / `multilingual` / `typed-decisions`).
    pub model: Option<String>,
    /// Approximate input-token budget for one decision request (state plus
    /// questions together). Over-budget requests are trimmed before
    /// sending — longest string fields first, each cut marked in place —
    /// because the Jev family runs small context windows (laya's shipped
    /// checkpoints read 512, 1024, or up to 8192 tokens) and an overflowing
    /// request is either rejected or silently cut past its questions. The
    /// estimate is a heuristic (roughly 3 characters per token, one per CJK
    /// character), not a tokenizer. The default 512 matches the smallest
    /// context laya ships (the `english` checkpoint); raise it for wider
    /// checkpoints (e.g. 8192 for `laya-multilingual` served with
    /// `max_len=8192`).
    #[serde(deserialize_with = "flex_uint")]
    pub max_input_tokens: usize,
}

impl Default for DecisionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: DecisionProviderConfig::LayaServe,
            endpoint: None,
            api_key_env: None,
            timeout_ms: 3000,
            model: None,
            max_input_tokens: 512,
        }
    }
}

impl DecisionConfig {
    /// The base URL to call: the configured endpoint, or the provider default.
    /// `None` when neither exists (an enabled `jev` with no explicit endpoint).
    pub fn endpoint(&self) -> Option<&str> {
        self.endpoint
            .as_deref()
            .or_else(|| self.provider.default_endpoint())
    }

    /// The environment variable name to read the bearer token from.
    pub fn api_key_env(&self) -> &str {
        self.api_key_env
            .as_deref()
            .unwrap_or_else(|| self.provider.default_api_key_env())
    }

    /// The per-request timeout.
    pub fn timeout(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.timeout_ms as u64)
    }
}

/// The Jev-compatible endpoint families argosy knows a default for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DecisionProviderConfig {
    /// A local `laya-serve` (the `laya[serve]` HTTP server). Default.
    #[default]
    LayaServe,
    /// Any other Jev-compatible endpoint, including TypeSafe Jev. Requires an
    /// explicit `endpoint`.
    Jev,
}

impl DecisionProviderConfig {
    /// The provider's default base URL, if it has one.
    fn default_endpoint(self) -> Option<&'static str> {
        match self {
            Self::LayaServe => Some("http://127.0.0.1:8000"),
            // No known default for the hosted Jev API: require an endpoint.
            Self::Jev => None,
        }
    }

    /// The provider's default API-key environment variable name.
    fn default_api_key_env(self) -> &'static str {
        match self {
            Self::LayaServe => "LAYA_API_KEY",
            Self::Jev => "JEV_API_KEY",
        }
    }
}

/// The configurable artifact format.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PackageFormatConfig {
    /// A plain directory tree.
    #[default]
    Dir,
    /// A gzipped tar archive.
    #[serde(rename = "tar.gz")]
    TarGz,
}

/// The configuration search root: `$XDG_CONFIG_HOME/argosy` (falling
/// back to `~/.config/argosy`; on Windows, where neither
/// `$XDG_CONFIG_HOME` nor `$HOME` is set by default,
/// `%USERPROFILE%\.config\argosy`). A sibling single file
/// `<...>/argosy.bml` is also part of the search (see [`Config::load`]).
pub fn config_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute() && !p.as_os_str().is_empty())
        .or_else(|| {
            std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(|home| PathBuf::from(home).join(".config"))
        })?;
    Some(base.join("argosy"))
}

impl Config {
    /// Loads the user configuration, or the all-defaults [`Config`] when
    /// no configuration files exist.
    pub fn load() -> Result<Self> {
        let dir = config_dir();
        Self::load_from(dir.as_deref())
    }

    /// [`Config::load`] against an explicit search root (tests and hosts
    /// inject a tempdir instead of touching `~/.config`): the single file
    /// `<root>.bml` — i.e. the root without its final `argosy` component,
    /// plus `.bml` — then every `*.bml` file under `<root>/`, in sorted
    /// name order. `None` means no configuration was found at all.
    pub fn load_from(root: Option<&Path>) -> Result<Self> {
        let Some(root) = root else {
            return Ok(Self::default());
        };
        let mut loader = StandardLoader::default();
        let mut found = false;

        // The single-file spelling: ~/.config/argosy.bml.
        let single = root.with_extension("bml");
        if single.is_file() {
            loader.add_file(&single).context(ConfigSnafu)?;
            found = true;
        }

        // The directory spelling: every *.bml under ~/.config/argosy/.
        if root.is_dir() {
            let mut files: Vec<PathBuf> = fs::read_dir(root)
                .map_err(|source| crate::error::Error::Io {
                    path: root.to_path_buf(),
                    source,
                })?
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.path())
                .filter(|path| {
                    path.is_file()
                        && path
                            .extension()
                            .is_some_and(|ext| ext.eq_ignore_ascii_case("bml"))
                })
                .collect();
            files.sort();
            for file in files {
                loader.add_file(&file).context(ConfigSnafu)?;
                found = true;
            }
        }

        if !found {
            return Ok(Self::default());
        }
        let statement = loader.load().context(ConfigSnafu)?;
        let config = barkml::de::from_statement::<Self>(&statement).context(ConfigSnafu)?;
        config.validate()?;
        Ok(config)
    }

    /// Rejects values that would silently corrupt later operations.
    pub fn validate(&self) -> Result<()> {
        if self.index.default_k == 0 || self.index.mcp_default_k == 0 {
            return Err(crate::error::Error::Validation {
                reason: "configuration: index.default_k and index.mcp_default_k must be >= 1"
                    .to_string(),
            });
        }
        // The default embedder truncates at 256 tokens; a chunk budget above
        // that would silently re-introduce the unsearchable tail chunking is
        // meant to remove.
        if !(1..=256).contains(&self.index.chunk_max_tokens) {
            return Err(crate::error::Error::Validation {
                reason: "configuration: index.chunk_max_tokens must be between 1 and 256 \
                         (the default embedder's truncation limit)"
                    .to_string(),
            });
        }
        if let Some(name) = &self.paths.index_db_name
            && (name.is_empty()
                || name.contains('/')
                || name.contains('\\')
                || name == "."
                || name == "..")
        {
            return Err(crate::error::Error::Validation {
                reason: format!(
                    "configuration: paths.index_db_name must be a plain file name, not `{name}`"
                ),
            });
        }
        #[cfg(feature = "default-index")]
        if let Some(model) = &self.index.model
            && crate::index::tract::ModelSpec::from_name(model).is_none()
        {
            let known: Vec<&str> = crate::index::tract::ModelSpec::ALL
                .iter()
                .map(|spec| spec.name())
                .collect();
            return Err(crate::error::Error::Validation {
                reason: format!(
                    "configuration: unknown index.model `{model}` (known models: {})",
                    known.join(", ")
                ),
            });
        }
        if self.session.project_ttl_secs == 0 || self.session.review_ttl_secs == 0 {
            return Err(crate::error::Error::Validation {
                reason: "configuration: session.project_ttl_secs and session.review_ttl_secs must be >= 1"
                    .to_string(),
            });
        }
        if self.session.project_max == 0 || self.session.review_max == 0 {
            return Err(crate::error::Error::Validation {
                reason: "configuration: session.project_max and session.review_max must be >= 1"
                    .to_string(),
            });
        }
        if self.decision.timeout_ms == 0 {
            return Err(crate::error::Error::Validation {
                reason: "configuration: decision.timeout_ms must be >= 1".to_string(),
            });
        }
        if self.decision.max_input_tokens == 0 {
            return Err(crate::error::Error::Validation {
                reason: "configuration: decision.max_input_tokens must be >= 1".to_string(),
            });
        }
        if self.decision.enabled && self.decision.endpoint().is_none() {
            return Err(crate::error::Error::Validation {
                reason:
                    "configuration: decision.endpoint is required when decision.enabled is set \
                         and the provider has no default (provider `jev`)"
                        .to_string(),
            });
        }
        if let Some(endpoint) = self.decision.endpoint()
            && !(endpoint.starts_with("http://") || endpoint.starts_with("https://"))
        {
            return Err(crate::error::Error::Validation {
                reason: format!(
                    "configuration: decision.endpoint must be an http(s) URL, not `{endpoint}`"
                ),
            });
        }
        for name in self.trust.keys() {
            if !crate::bundle::is_safe_bundle_name(name) {
                return Err(crate::error::Error::Validation {
                    reason: format!(
                        "configuration: trust key `{name}` is not a valid argosy manifest name \
                         (only [A-Za-z0-9._-] are allowed — the key must spell the import's \
                         manifest name, the `argosy://` URI spelling)"
                    ),
                });
            }
        }
        Ok(())
    }

    /// The provenance trust policy derived from the `trust` section: the
    /// maximum OKF tier each named import may carry. Imports with no entry
    /// are capped at `unverified` whatever their concepts claim.
    pub fn trust_policy(&self) -> TrustPolicy {
        TrustPolicy::from_caps(self.trust.clone())
    }

    /// The configured embedding model's registry name (default included).
    pub fn model_name(&self) -> &str {
        self.index.model.as_deref().unwrap_or("all-minilm-l6-v2")
    }

    /// The configured embedding cache, with `~` expanded.
    pub fn embed_cache_dir(&self) -> Option<PathBuf> {
        self.paths.embed_cache_dir.as_deref().and_then(expand_home)
    }

    /// The index file name (default `index.db`).
    pub fn index_db_name(&self) -> &str {
        self.paths.index_db_name.as_deref().unwrap_or("index.db")
    }

    /// The per-passage token budget (`index.chunk_max_tokens`).
    pub fn chunk_config(&self) -> crate::index::ChunkConfig {
        crate::index::ChunkConfig {
            max_tokens: self.index.chunk_max_tokens,
        }
    }
}

/// Expands a leading `~` to the user's home directory.
fn expand_home(path: &Path) -> Option<PathBuf> {
    let text = path.to_str()?;
    if text == "~" {
        return home();
    }
    let rest = text.strip_prefix("~/")?;
    Some(home()?.join(rest))
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// The user's home directory (`$HOME`, falling back to `%USERPROFILE%`).
/// Shared by configuration `~` expansion and catalog path redaction.
pub fn home_dir() -> Option<PathBuf> {
    home()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(path: &Path, body: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    fn load(root: &Path) -> Config {
        Config::load_from(Some(root)).unwrap()
    }

    #[test]
    fn missing_config_is_all_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(load(tmp.path()), Config::default());
        assert_eq!(Config::load_from(None).unwrap(), Config::default());
    }

    #[test]
    fn single_file_overrides_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            &tmp.path().join("argosy.bml"),
            r#"
            output { quiet = true }
            pull { default_global = true }
            "#,
        );
        let config = load(&tmp.path().join("argosy"));
        assert!(config.output.quiet);
        assert!(config.pull.default_global);
        assert_eq!(config.index.default_k, 5);
        assert_eq!(config.index.mcp_default_k, 8);
    }

    #[test]
    fn directory_files_combine_into_one_config() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("argosy");
        write(&root.join("a.bml"), "index { default_k = 9 }");
        write(&root.join("b.bml"), "output { json = true }");
        let config = load(&root);
        assert_eq!(config.index.default_k, 9);
        assert!(config.output.json);
    }

    #[test]
    fn single_file_and_directory_files_all_load() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("argosy");
        write(&tmp.path().join("argosy.bml"), "index { default_k = 1 }");
        write(&root.join("z.bml"), "output { quiet = true }");
        let config = load(&root);
        assert_eq!(config.index.default_k, 1);
        assert!(config.output.quiet);
    }

    #[test]
    fn the_same_key_in_two_files_is_a_collision() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("argosy");
        write(&root.join("a.bml"), "index { default_k = 1 }");
        write(&root.join("b.bml"), "index { default_k = 7 }");
        assert!(Config::load_from(Some(&root)).is_err());
    }

    #[test]
    fn all_sections_parse() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("argosy");
        write(
            &root.join("argosy.bml"),
            r#"
            paths {
                embed_cache_dir = "~/.cache/custom-embeds"
                index_db_name = "my.index.db"
            }
            index { default_k = 12
                mcp_default_k = 20
                chunk_max_tokens = 128 }
            package { format = "tar.gz"
                include_index = true }

            "#,
        );
        let config = load(&root);
        assert_eq!(
            config.embed_cache_dir(),
            Some(home().unwrap().join(".cache/custom-embeds"))
        );
        assert_eq!(config.index_db_name(), "my.index.db");
        assert_eq!(config.index.default_k, 12);
        assert_eq!(config.index.mcp_default_k, 20);
        assert_eq!(config.index.chunk_max_tokens, 128);
        assert_eq!(config.chunk_config().max_tokens, 128);
        assert_eq!(config.package.format, PackageFormatConfig::TarGz);
        assert!(config.package.include_index);
    }

    #[test]
    fn session_defaults_are_bounded() {
        let session = Config::default().session;
        assert_eq!(session.project_ttl_secs, 1800);
        assert_eq!(session.project_max, 8);
        assert_eq!(session.review_ttl_secs, 7200);
        assert_eq!(session.review_max, 16);
    }

    #[test]
    fn session_section_parses() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            &tmp.path().join("argosy.bml"),
            r#"
            session {
                project_ttl_secs = 60
                project_max = 2
                review_ttl_secs = 120
                review_max = 4
            }
            "#,
        );
        let config = load(&tmp.path().join("argosy"));
        assert_eq!(config.session.project_ttl_secs, 60);
        assert_eq!(config.session.project_max, 2);
        assert_eq!(config.session.review_ttl_secs, 120);
        assert_eq!(config.session.review_max, 4);
        assert_eq!(
            config.session.project_ttl(),
            std::time::Duration::from_secs(60)
        );
        assert_eq!(
            config.session.review_ttl(),
            std::time::Duration::from_secs(120)
        );
    }

    #[test]
    fn zero_session_bounds_are_rejected() {
        let mut config = Config::default();
        config.session.project_ttl_secs = 0;
        assert!(config.validate().is_err());
        config.session.project_ttl_secs = 1;
        config.session.review_ttl_secs = 0;
        assert!(config.validate().is_err());
        config.session.review_ttl_secs = 1;
        config.session.project_max = 0;
        assert!(config.validate().is_err());
        config.session.project_max = 1;
        config.session.review_max = 0;
        assert!(config.validate().is_err());
        config.session.review_max = 1;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        write(&tmp.path().join("argosy.bml"), "outputs { quiet = true }");
        let err = Config::load_from(Some(&tmp.path().join("argosy"))).unwrap_err();
        assert!(format!("{err:#}").contains("configuration"));
    }

    #[test]
    fn zero_k_is_rejected() {
        let mut config = Config::default();
        config.index.default_k = 0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn search_mode_parses_from_config_and_names_its_invariants() {
        // Default is hybrid; both names load; anything else is a config
        // error, never a silent default.
        assert_eq!(
            Config::default().index.search_mode,
            SearchModeConfig::Hybrid
        );
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("argosy");
        write(&root.join("a.bml"), "index { search_mode = \"lexical\" }");
        let config = load(&root);
        assert_eq!(config.index.search_mode, SearchModeConfig::Lexical);
        write(
            &tmp.path().join("argosy.bml"),
            "index { search_mode = \"vector\" }",
        );
        let err = Config::load_from(Some(&tmp.path().join("argosy"))).unwrap_err();
        assert!(
            format!("{err:#}").contains("search_mode"),
            "the error names the unknown value: {err:#}"
        );
    }

    #[test]
    fn chunk_budget_is_bounded_by_the_embedder_limit() {
        assert_eq!(Config::default().index.chunk_max_tokens, 220);
        assert_eq!(
            Config::default().chunk_config(),
            crate::index::ChunkConfig { max_tokens: 220 }
        );

        let mut config = Config::default();
        config.index.chunk_max_tokens = 0;
        assert!(config.validate().is_err(), "0 is not a budget");
        config.index.chunk_max_tokens = 257;
        let err = config.validate().unwrap_err();
        assert!(
            err.to_string().contains("chunk_max_tokens"),
            "names the key: {err}"
        );
        config.index.chunk_max_tokens = 256;
        assert!(config.validate().is_ok(), "the truncation limit is allowed");
    }

    #[test]
    fn model_name_parses_and_unknown_models_are_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            &tmp.path().join("argosy.bml"),
            "index { model = \"all-minilm-l6-v2\" }",
        );
        let config = load(&tmp.path().join("argosy"));
        assert_eq!(config.model_name(), "all-minilm-l6-v2");
        assert_eq!(Config::default().model_name(), "all-minilm-l6-v2");

        let mut config = Config::default();
        config.index.model = Some("giant-lm-9000".into());
        let err = config.validate().unwrap_err();
        assert!(format!("{err}").contains("unknown index.model"));
    }

    #[test]
    fn path_like_index_db_name_is_rejected() {
        let mut config = Config::default();
        config.paths.index_db_name = Some("nested/index.db".into());
        assert!(config.validate().is_err());
    }

    #[test]
    fn malformed_bml_is_a_config_error() {
        let tmp = tempfile::tempdir().unwrap();
        write(&tmp.path().join("argosy.bml"), "this is { not barkml");
        assert!(Config::load_from(Some(&tmp.path().join("argosy"))).is_err());
    }

    #[test]
    fn decision_defaults_are_disabled_laya_serve() {
        let config = Config::default();
        assert!(!config.decision.enabled);
        assert_eq!(config.decision.provider, DecisionProviderConfig::LayaServe);
        assert_eq!(config.decision.endpoint(), Some("http://127.0.0.1:8000"));
        assert_eq!(config.decision.api_key_env(), "LAYA_API_KEY");
        assert_eq!(config.decision.timeout_ms, 3000);
        assert_eq!(config.decision.max_input_tokens, 512);
    }

    #[test]
    fn code_tools_section_parses_and_defaults_to_confined() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            &tmp.path().join("argosy.bml"),
            "code_tools { allow_external_paths = true }",
        );
        let config = load(&tmp.path().join("argosy"));
        assert!(config.code_tools.allow_external_paths);
        assert!(!Config::default().code_tools.allow_external_paths);
    }

    #[test]
    fn trust_section_parses_into_provenance_caps() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            &tmp.path().join("argosy.bml"),
            r#"
            trust {
                "acme-shared" = "machine-confirmed"
                "acme-vetted" = "human-reviewed"
            }
            "#,
        );
        let config = load(&tmp.path().join("argosy"));
        let policy = config.trust_policy();
        assert_eq!(policy.cap("acme-shared"), TrustTier::MachineConfirmed);
        assert_eq!(policy.cap("acme-vetted"), TrustTier::HumanReviewed);
        // An import with no configured entry caps at unverified, and the
        // default configuration grants nothing.
        assert_eq!(policy.cap("anything-else"), TrustTier::Unverified);
        assert_eq!(
            Config::default().trust_policy().cap("acme-shared"),
            TrustTier::Unverified
        );
    }

    #[test]
    fn trust_rejects_unknown_tier_values() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            &tmp.path().join("argosy.bml"),
            r#"trust { "acme-shared" = "trust-me" }"#,
        );
        let err = Config::load_from(Some(&tmp.path().join("argosy"))).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("trust") || msg.contains("tier"), "{msg}");
    }

    #[test]
    fn trust_rejects_keys_that_cannot_be_manifest_names() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            &tmp.path().join("argosy.bml"),
            r#"trust { "acme shared" = "human-reviewed" }"#,
        );
        let err = Config::load_from(Some(&tmp.path().join("argosy"))).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("trust key"), "{msg}");
    }

    #[test]
    fn decision_section_parses() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            &tmp.path().join("argosy.bml"),
            r#"
            decision {
                enabled = true
                provider = "jev"
                endpoint = "https://jev.example.com/"
                api_key_env = "JEV_KEY"
                timeout_ms = 5000
                model = "typed-decisions"
                max_input_tokens = 2048
            }
            "#,
        );
        let config = load(&tmp.path().join("argosy"));
        assert!(config.decision.enabled);
        assert_eq!(config.decision.provider, DecisionProviderConfig::Jev);
        assert_eq!(config.decision.endpoint(), Some("https://jev.example.com/"));
        assert_eq!(config.decision.api_key_env(), "JEV_KEY");
        assert_eq!(config.decision.timeout_ms, 5000);
        assert_eq!(config.decision.model.as_deref(), Some("typed-decisions"));
        assert_eq!(config.decision.max_input_tokens, 2048);
    }

    #[test]
    fn decision_validation_requires_endpoint_scheme_for_jev_and_positive_timeout() {
        let mut config = Config::default();
        config.decision.enabled = true;
        config.decision.provider = DecisionProviderConfig::Jev;
        // An enabled `jev` has no default endpoint.
        assert!(config.validate().is_err());
        config.decision.endpoint = Some("ftp://nope".into());
        assert!(config.validate().is_err());
        config.decision.endpoint = Some("https://ok.example.com".into());
        assert!(config.validate().is_ok());
        config.decision.timeout_ms = 0;
        assert!(config.validate().is_err());
        config.decision.timeout_ms = 3000;
        config.decision.max_input_tokens = 0;
        assert!(config.validate().is_err());
    }
}
