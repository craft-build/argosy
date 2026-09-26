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
//!     default_k = 5
//!     mcp_default_k = 8
//! }
//! package {
//!     format = "dir"        // or "tar.gz"
//!     include_index = false
//! }
//! catalog {
//!     redact_home = false
//! }
//! ```
//!
//! Paths honor `$XDG_CONFIG_HOME` (falling back to `~/.config`). Location
//! of argosy state and cache data stays with the standard environment
//! (`$XDG_STATE_HOME`, `$ARGOSY_EMBED_CACHE_DIR`) except where this
//! config explicitly overrides it.

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
    /// Default hit count for `argosy index query`.
    #[serde(deserialize_with = "flex_uint")]
    pub default_k: usize,
    /// Default hit count for the MCP `search`/`search_rules` tools.
    #[serde(deserialize_with = "flex_uint")]
    pub mcp_default_k: usize,
}

impl Default for IndexConfig {
    fn default() -> Self {
        Self {
            model: None,
            default_k: 5,
            mcp_default_k: 8,
        }
    }
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
        if let Some(name) = &self.paths.index_db_name {
            if name.is_empty()
                || name.contains('/')
                || name.contains('\\')
                || name == "."
                || name == ".."
            {
                return Err(crate::error::Error::Validation {
                    reason: format!(
                        "configuration: paths.index_db_name must be a plain file name, not `{name}`"
                    ),
                });
            }
        }
        #[cfg(feature = "default-index")]
        if let Some(model) = &self.index.model {
            if crate::index::tract::ModelSpec::from_name(model).is_none() {
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
        }
        Ok(())
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
                mcp_default_k = 20 }
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
        assert_eq!(config.package.format, PackageFormatConfig::TarGz);
        assert!(config.package.include_index);
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
}
