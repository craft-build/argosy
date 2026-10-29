//! The MCP server: argosys as MCP resources and tools for any
//! MCP-compatible harness. Stdout *is* the protocol channel — a stray
//! `println!` corrupts it; diagnostics go to stderr. Tool calls name
//! their project with `cwd`; resources resolve against the process
//! working directory.

mod params;
mod reports;
mod session;

mod prompts;
mod server;
mod tools;

pub use params::*;
pub use prompts::{get_prompt_result, prompt_definitions};
pub use reports::*;
pub use server::{ArgosyMcpServer, resource_templates};
pub use session::{McpState, ProjectSession, SessionFactory};
pub use tools::{Toolset, Toolsets, tool_definitions, tool_definitions_for, toolset_of};

/// The `argosy://_argosys` pseudo-resource: the active argosys.
pub const ARGOSYS_URI: &str = "argosy://_argosys";

/// The `argosy://catalog` pseudo-resource (see [`crate::catalog`]).
pub const CATALOG_URI: &str = "argosy://catalog";

/// Suffix of the `argosy://<name>/_index` root-index pseudo-resource.
pub const ARGOSY_INDEX_SUFFIX: &str = "/_index";

/// The RFC 6570 URI template for concept resources, advertised through
/// `list_resource_templates` (`{+id}` allows nested concept paths).
pub const CONCEPT_TEMPLATE_URI: &str = "argosy://{argosy}/{namespace}/{+id}";

/// Default hit count for `search`/`search_rules`.
pub(super) const DEFAULT_K: usize = 8;

/// Default project-session idle TTL.
pub(super) const DEFAULT_PROJECT_TTL: std::time::Duration = std::time::Duration::from_secs(1800);

/// Default project-session cap.
pub(super) const DEFAULT_MAX_PROJECT_SESSIONS: usize = 8;

/// The trust tier reported for concepts with no `verified` frontmatter ([`crate::trust`]).
pub(super) use crate::trust::UNVERIFIED;

#[cfg(test)]
mod tests;
