//! The MCP server: argosys as MCP resources and tools for any
//! MCP-compatible harness. Stdout *is* the protocol channel — one stray
//! `println!` corrupts it; diagnostics go to stderr. [`McpState`] holds the
//! sync, unit-testable handlers; [`ArgosyMcpServer`] dispatches.
//!
//! Multi-project: every tool call names its project with `cwd` (the project
//! root); [`McpState`] opens each project once through its
//! [`SessionFactory`] and caches by canonical root. Resources (which carry
//! no cwd) resolve against the process working directory, opened the same
//! lazy way.

mod params;
mod reports;
mod session;

mod prompts;
mod server;
mod tools;

pub use params::*;
pub use prompts::{get_prompt_result, prompt_definitions};
pub use reports::*;
pub use server::ArgosyMcpServer;
pub use session::{McpState, ProjectSession, SessionFactory};
pub use tools::tool_definitions;

/// The `argosy://_argosys` pseudo-resource: the active argosys with their
/// versions and local/imported roles.
pub const ARGOSYS_URI: &str = "argosy://_argosys";

/// Suffix of the `argosy://<name>/_index` pseudo-resource: a bundle's root
/// `index.md`.
pub const ARGOSY_INDEX_SUFFIX: &str = "/_index";

/// Default hit count for `search`/`search_rules`.
pub(super) const DEFAULT_K: usize = 8;

/// The trust tier reported for concepts with no `verified` frontmatter.
pub(super) const UNVERIFIED: &str = "unverified";

#[cfg(test)]
mod tests;
