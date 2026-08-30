//! Tool parameters: `Deserialize` for dispatch, `JsonSchema` for listings.

use std::path::PathBuf;

// The `JsonSchema` derive expansion references the `schemars` crate by
// name; aliasing rmcp's re-export keeps our version pinned to the SDK's.
use rmcp::schemars;
use serde::Deserialize;

use crate::concept::{Concept, ConceptId};
use crate::error::Result;

/// `search` parameters.
#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
pub struct SearchParams {
    /// The project root to operate on.
    pub cwd: PathBuf,
    /// Natural-language query: semantic match against every indexed concept.
    pub query: String,
    /// Maximum hits to return (default 8).
    pub k: Option<usize>,
    /// Restrict to these namespaces (`document`, `skill`, `memory`,
    /// `styleguide`); unknown spellings error rather than matching nothing.
    pub namespaces: Option<Vec<String>>,
    /// Restrict to one argosy by manifest name; unknown names error.
    pub argosy: Option<String>,
    /// Restrict to concepts carrying any of these tags.
    pub tags: Option<Vec<String>>,
    /// Restrict to concepts with this frontmatter `type`.
    pub r#type: Option<String>,
    /// Restrict to concepts with this exact `language` facet.
    pub language: Option<String>,
    /// Restrict to concepts with this exact `category` facet.
    pub category: Option<String>,
}

/// `search_rules` parameters.
#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
pub struct RulesParams {
    /// The project root to operate on.
    pub cwd: PathBuf,
    /// Natural-language description of the code or review concern.
    pub query: String,
    /// Restrict to rules with this `language` facet (e.g. `rust`).
    pub language: Option<String>,
    /// Restrict to rules with this `category` facet (e.g. `naming`).
    pub category: Option<String>,
    /// Maximum hits to return (default 8).
    pub k: Option<usize>,
}

/// `list_skills` parameters.
#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
pub struct ListSkillsParams {
    /// The project root to operate on.
    pub cwd: PathBuf,
}

/// `get_skill` parameters.
#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
pub struct GetSkillParams {
    /// The project root to operate on.
    pub cwd: PathBuf,
    /// The skill's name (entry-point file stem), resolved by precedence
    /// across all active argosies.
    pub name: String,
}

/// Single-path read/delete parameters (`read_memory`, `delete_memory`,
/// `delete_rule`, `delete_document`).
#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
pub struct ReadPathParams {
    /// The project root to operate on.
    pub cwd: PathBuf,
    /// Bundle-relative concept path including the namespace prefix, e.g.
    /// `memory/gotchas`.
    pub path: String,
}

/// `read` parameters: one concept from any active argosy.
#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
pub struct ReadParams {
    /// The project root to operate on.
    pub cwd: PathBuf,
    /// Bundle-relative concept path including a reserved-namespace prefix,
    /// e.g. `memory/gotchas`.
    pub path: String,
    /// Argosy manifest name to read from; defaults to the local argosy.
    /// Unknown names error.
    pub argosy: Option<String>,
}

/// Write parameters (`write_memory`, `write_rule`, `write_document`).
#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
pub struct WriteParams {
    /// The project root to operate on.
    pub cwd: PathBuf,
    /// Bundle-relative concept path including the namespace prefix;
    /// namespace-contract violations are rejected.
    pub path: String,
    /// Full concept content: YAML frontmatter followed by the markdown body.
    pub content: String,
}

/// Styleguide promotion targets accepted by the `promote` tool.
#[derive(Debug, Clone, Copy, Deserialize, rmcp::schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum PromoteTarget {
    /// Promote to a prose concept under `document/`.
    Document,
    /// Promote to a `Styleguide Rule` under `styleguide/`; requires a
    /// description — supply `description` unless the source has one.
    #[serde(rename = "styleguide")]
    StyleguideRule,
}

/// `promote` parameters.
#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
pub struct PromoteParams {
    /// The project root to operate on.
    pub cwd: PathBuf,
    /// Bundle-relative memory path to promote; the source is never moved
    /// or deleted.
    pub source_path: String,
    /// `"document"` or `"styleguide"`.
    pub target: PromoteTarget,
    /// Bundle-relative path of the new concept; must not already exist.
    pub new_path: String,
    /// Description override; required when `target` is `styleguide` and the
    /// source has no description of its own.
    pub description: Option<String>,
}

/// Parses a bundle-relative concept path into a [`ConceptId`], surfacing the
/// library's spelling rules as a tool argument error.
pub(super) fn concept_id(path: &str) -> Result<ConceptId> {
    path.parse()
}

/// Parses full markdown-with-frontmatter into a [`Concept`].
pub(super) fn parse_concept(content: &str) -> Result<Concept> {
    content.parse()
}
