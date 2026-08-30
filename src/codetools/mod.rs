//! Workspace code-intelligence tools, ported from Craft: `outline`, `zoom`,
//! `astgrep`, `conflicts`, `inspect`, `callgraph`, and `repomap`.
//!
//! These tools know nothing about argosy bundles — they operate on the
//! workspace filesystem the host process was spawned in, which makes them a
//! natural companion to the knowledge tools for any coding harness. Handlers
//! are **synchronous, unit-testable functions** of `(&CodeTools, Params)`
//! (the same layering discipline as `McpState`); the MCP layer in
//! [`crate::mcp`] dispatches them on the blocking pool.
//!
//! **stdout discipline**: when served over MCP, stdout is the protocol
//! channel. These modules never print; the only diagnostics are `tracing`
//! calls, which are no-ops without a subscriber.
//!
//! Port notes: Craft's async `FsBackend` indirection, permission scopes, and
//! tool registry are dropped; everything else keeps Craft's structure and
//! names so future diffs stay readable.

pub mod astgrep;
pub mod callgraph;
pub mod conflicts;
mod file_tracker;
pub mod inspect;
pub mod outline;
pub mod repomap;
pub mod zoom;

pub use file_tracker::FileReadTracker;
pub use repomap::RepoMap;

use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use crate::error::{Error, Result};

/// Process cwd, captured once at first use — the anchor for relative paths
/// and for display-shortening (Craft does the same).
static CWD: LazyLock<Option<PathBuf>> = LazyLock::new(|| env::current_dir().ok());

/// `$HOME`, for `~` expansion and `~/` display shortening.
static HOME: LazyLock<Option<PathBuf>> = LazyLock::new(|| {
    env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
});

/// Shared state for the code tools: the stale-read guard and one cached
/// [`RepoMap`] per root. Everything is interior-mutable, so a single
/// `Arc<CodeTools>` is shared by every tool call without locking the argosy
/// state.
#[derive(Default)]
pub struct CodeTools {
    /// Files read through these tools, by (canonical path, mtime). A write
    /// (`astgrep` apply, `conflicts` resolve) is refused when the file
    /// changed since the last read.
    tracker: FileReadTracker,
    /// One `RepoMap` per canonical root so the tags/render caches survive
    /// across calls (they are signature-checked against disk mtimes anyway).
    repo_maps: Mutex<HashMap<PathBuf, RepoMap>>,
    /// Serializes mutating runs (`astgrep` apply, `conflicts` resolve). The
    /// stale-read guard alone is check-then-act over shared files: two
    /// concurrent applies both pass the check and the second write silently
    /// wins. Holding this lock across a whole mutating run closes that
    /// window; read-only runs never take it.
    write_lock: Mutex<()>,
}

impl CodeTools {
    /// Records a read of `path` for the stale-read guard.
    pub fn record_read(&self, path: &Path) {
        self.tracker.record_read(path);
    }

    /// Refuses an edit of `path` when it changed since the last recorded
    /// read; files never read (or since deleted) are always allowed.
    pub fn check_before_edit(&self, path: &Path) -> Result<()> {
        self.tracker
            .check_before_edit(path)
            .map_err(|message| Error::CodeTool { message })
    }

    /// Takes the mutating-run lock for the duration of the returned guard —
    /// mutating handlers call this before touching files so concurrent
    /// mutating runs serialize instead of racing their check-then-write.
    pub(crate) fn begin_write(&self) -> std::sync::MutexGuard<'_, ()> {
        self.write_lock.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The cached [`RepoMap`] for `root`, creating it on first use. A
    /// non-default `max_tokens` budget returns an uncached instance (the
    /// cache is budget-less); `refresh` drops the cached instance's caches
    /// first.
    pub fn repomap_for_root(&self, root: &Path, max_tokens: Option<u32>, refresh: bool) -> RepoMap {
        if let Some(max_tokens) = max_tokens {
            return RepoMap::new(root).with_max_tokens(max_tokens);
        }
        let key = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let mut maps = self.repo_maps.lock().unwrap_or_else(|e| e.into_inner());
        if refresh && let Some(map) = maps.get(&key) {
            map.force_refresh();
        }
        maps.entry(key)
            .or_insert_with(|| RepoMap::new(root))
            .clone()
    }
}

/// Builds a tool error from any message, phrased for the LLM caller.
pub(crate) fn tool_error(message: impl Into<String>) -> Error {
    Error::CodeTool {
        message: message.into(),
    }
}

/// Resolves a user-supplied path: `~` expansion, then relative paths are
/// anchored to the process cwd. Ported from Craft's `tools::resolve_path`.
pub(crate) fn resolve_path(path: &str) -> Result<String> {
    let expanded = if let Some(rest) = path.strip_prefix("~/") {
        let home = HOME
            .as_deref()
            .ok_or_else(|| tool_error("cannot expand ~: HOME not set"))?;
        home.join(rest).to_string_lossy().into_owned()
    } else if path == "~" {
        let home = HOME
            .as_deref()
            .ok_or_else(|| tool_error("cannot expand ~: HOME not set"))?;
        home.to_string_lossy().into_owned()
    } else {
        path.to_string()
    };

    if Path::new(&expanded).is_relative() {
        let cwd = env::current_dir().map_err(|e| tool_error(format!("cwd error: {e}")))?;
        Ok(cwd.join(&expanded).to_string_lossy().into_owned())
    } else {
        Ok(expanded)
    }
}

/// Display form of a path: cwd-relative when inside it, `~/`-relative when
/// inside home, verbatim otherwise. Ported from Craft's
/// `tools::relative_path`.
pub(crate) fn relative_path(path: &str) -> String {
    let p = Path::new(path);
    if let Some(cwd) = CWD.as_deref()
        && let Ok(rel) = p.strip_prefix(cwd)
    {
        return format_rel("", ".", rel);
    }
    if let Some(home) = HOME.as_deref()
        && let Ok(rel) = p.strip_prefix(home)
    {
        return format_rel("~/", "~", rel);
    }
    path.to_string()
}

fn format_rel(prefix: &str, fallback: &str, rel: &Path) -> String {
    let s = rel.to_string_lossy();
    if s.is_empty() {
        fallback.into()
    } else {
        format!("{prefix}{s}")
    }
}

/// `.git` is always excluded, even when `gitignore` is false. Ported from
/// Craft's `tools::walk_builder_opts`.
pub(crate) fn walk_builder_opts(
    root: &str,
    patterns: &[&str],
    gitignore: bool,
) -> Result<ignore::WalkBuilder> {
    let mut ob = ignore::overrides::OverrideBuilder::new(root);
    ob.add("!.git").expect("!.git is a valid glob");

    for p in patterns {
        ob.add(p)
            .map_err(|e| tool_error(format!("invalid glob pattern: {e}")))?;
    }

    let overrides = ob
        .build()
        .map_err(|e| tool_error(format!("invalid glob pattern: {e}")))?;

    let mut wb = ignore::WalkBuilder::new(root);
    wb.hidden(false).overrides(overrides);
    if !gitignore {
        wb.ignore(false)
            .git_ignore(false)
            .git_global(false)
            .git_exclude(false);
    }
    Ok(wb)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_path_inside_cwd_shortens() {
        let cwd = env::current_dir().unwrap();
        let joined = cwd.join("src/lib.rs").to_string_lossy().into_owned();
        assert_eq!(relative_path(&joined), "src/lib.rs");
        assert_eq!(relative_path(cwd.to_str().unwrap()), ".");
    }

    #[test]
    fn relative_path_outside_cwd_is_verbatim() {
        assert_eq!(
            relative_path("/definitely/not/here/x.rs"),
            "/definitely/not/here/x.rs"
        );
    }

    #[test]
    fn resolve_path_relative_anchors_to_cwd() {
        let resolved = resolve_path("Cargo.toml").unwrap();
        let cwd = env::current_dir().unwrap();
        assert_eq!(Path::new(&resolved), cwd.join("Cargo.toml"));
    }

    #[test]
    fn repomap_cache_reuses_one_instance_per_root() {
        let tools = CodeTools::default();
        let dir = tempfile::tempdir().unwrap();
        let a = tools.repomap_for_root(dir.path(), None, false);
        let b = tools.repomap_for_root(dir.path(), None, false);
        // Same cached instance (Arc-shared caches): equal render for equal input.
        let text_a = a.get_repo_map(&[], &[], "query");
        let text_b = b.get_repo_map(&[], &[], "query");
        assert_eq!(text_a, text_b);
        // A custom budget must not poison the default-budget cache.
        let _custom = tools.repomap_for_root(dir.path(), Some(64), false);
        let c = tools.repomap_for_root(dir.path(), None, false);
        assert_eq!(c.get_repo_map(&[], &[], "query"), text_a);
    }
}
