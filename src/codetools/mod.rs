//! Workspace code-intelligence tools: `outline`, `zoom`, `astgrep`, `conflicts`,
//! `inspect`, `callgraph`, `repomap`, and review sessions.
//!
//! Handlers are synchronous (the MCP layer dispatches them on the blocking
//! pool), every path parameter stays inside the workspace root unless the user
//! opts out, and these modules never print — stdout is the MCP protocol channel.

pub mod astgrep;
pub mod callgraph;
pub mod conflicts;
mod diffhunks;
mod file_tracker;
pub mod inspect;
pub mod outline;
pub mod repomap;
pub mod review;
mod review_export;
pub mod zoom;

pub use file_tracker::FileReadTracker;
pub use repomap::RepoMap;

use std::collections::HashMap;
use std::env;
use std::path::{Component, Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use crate::error::{Error, Result};

/// Process cwd captured once at first use: the anchor for relative paths and display shortening.
static CWD: LazyLock<Option<PathBuf>> = LazyLock::new(|| env::current_dir().ok());

/// `$HOME`, for `~` expansion and `~/` display shortening.
static HOME: LazyLock<Option<PathBuf>> = LazyLock::new(|| {
    env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
});

/// Shared state for the code tools: the stale-read guard, cached
/// [`RepoMap`]s, and the workspace-root containment boundary.
pub struct CodeTools {
    /// Files read through these tools; a write is refused if the file is stale or unread.
    tracker: FileReadTracker,
    /// One cached `RepoMap` per canonical root.
    repo_maps: Mutex<HashMap<PathBuf, RepoMap>>,
    /// Serializes mutating runs (`astgrep` apply, `conflicts` resolve): the
    /// stale-read guard alone is check-then-act, so two applies could both pass.
    write_lock: Mutex<()>,
    /// Diff snapshots and findings, keyed by the opaque id returned to the caller.
    reviews: review::ReviewManager,
    /// Canonical workspace root (the process cwd by default) that every path must stay inside.
    root: PathBuf,
    /// User-config opt-out from root confinement (`code_tools.allow_external_paths`).
    allow_external_paths: bool,
}

impl Default for CodeTools {
    fn default() -> Self {
        Self {
            tracker: FileReadTracker::default(),
            repo_maps: Mutex::new(HashMap::new()),
            write_lock: Mutex::new(()),
            reviews: review::ReviewManager::default(),
            root: default_root(),
            allow_external_paths: false,
        }
    }
}

/// The containment root: the process cwd, canonicalized so symlinked spellings compare equal.
fn default_root() -> PathBuf {
    env::current_dir()
        .and_then(|cwd| cwd.canonicalize())
        .unwrap_or_else(|_| PathBuf::from("."))
}

impl CodeTools {
    /// Anchors the tools at `root` (canonicalized): relative paths resolve
    /// against it and absolute paths must stay inside it.
    pub fn with_root(mut self, root: impl AsRef<Path>) -> Self {
        let root = root.as_ref().to_path_buf();
        self.root = root.canonicalize().unwrap_or(root);
        self
    }

    /// The workspace root paths are confined to.
    pub fn workspace_root(&self) -> &Path {
        &self.root
    }

    /// Opts in to resolving paths outside the workspace root.
    pub fn with_external_paths(mut self, allow: bool) -> Self {
        self.allow_external_paths = allow;
        self
    }

    /// Bounds the review-session cache.
    pub fn with_review_limits(mut self, ttl: std::time::Duration, max_sessions: usize) -> Self {
        self.reviews = self.reviews.with_limits(ttl, max_sessions);
        self
    }

    /// Installs the project-grounding source for review sessions (see
    /// [`review::ReviewGrounding`]): compiled styleguide `pattern` rules
    /// for `start_review`'s deterministic pre-pass, and the valid
    /// `rule_uris` set for `report_finding` grounding checks. Without one,
    /// reviews run without the pre-pass and skip rule-URI existence checks.
    pub fn with_review_grounding(mut self, grounding: review::ReviewGrounding) -> Self {
        self.reviews = self.reviews.with_grounding(grounding);
        self
    }

    /// Resolves a user-supplied path (`~` expansion, root anchoring, then
    /// containment unless [`Self::with_external_paths`] opted out), returning
    /// the anchored form rather than the canonical one.
    pub(crate) fn resolve_path(&self, path: &str) -> Result<String> {
        let expanded = expand_tilde(path)?;
        let resolved = if Path::new(&expanded).is_relative() {
            self.root.join(&expanded)
        } else {
            PathBuf::from(&expanded)
        };
        if self.allow_external_paths {
            return Ok(resolved.to_string_lossy().into_owned());
        }
        // Canonicalize the longest existing prefix — a symlink inside the root
        // could point outside it — then normalize leftover `..`/`.` so a
        // lexical escape cannot hide behind a missing mid-component.
        let check = lexical_normalize(&canonicalize_prefix(&resolved));
        if !check.starts_with(&self.root) {
            return Err(tool_error(format!(
                "path escapes the workspace root {}: {} — pass a path inside the workspace, or \
                 set code_tools.allow_external_paths = true in the user config to opt out",
                self.root.display(),
                path
            )));
        }
        Ok(resolved.to_string_lossy().into_owned())
    }

    /// Records a read of `path` for the stale-read guard.
    pub fn record_read(&self, path: &Path) {
        self.tracker.record_read(path);
    }

    /// Refuses an edit of a stale or never-read file (fail closed); a missing file may be written.
    pub fn check_before_edit(&self, path: &Path) -> Result<()> {
        self.tracker
            .check_before_edit(path)
            .map_err(|message| Error::CodeTool { message })
    }

    /// Takes the mutating-run lock for the duration of the returned guard.
    pub(crate) fn begin_write(&self) -> std::sync::MutexGuard<'_, ()> {
        self.write_lock.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The cached [`RepoMap`] for `root`, creating it on first use.
    ///
    /// A non-default `max_tokens` returns an uncached instance; `refresh`
    /// drops the cached instance's caches first.
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

    /// Installs the process-global decision endpoint used for advisory
    /// review-finding scoring.
    #[cfg(feature = "decision")]
    pub fn with_decision(
        mut self,
        decision: std::sync::Arc<dyn crate::decision::DecisionProvider>,
    ) -> Self {
        self.reviews.set_decision(decision);
        self
    }
}

/// Builds a tool error from any message, phrased for the LLM caller.
pub(crate) fn tool_error(message: impl Into<String>) -> Error {
    Error::CodeTool {
        message: message.into(),
    }
}

/// Expands a leading `~`/`~/` to `$HOME`.
fn expand_tilde(path: &str) -> Result<String> {
    if let Some(rest) = path.strip_prefix("~/") {
        let home = HOME
            .as_deref()
            .ok_or_else(|| tool_error("cannot expand ~: HOME not set"))?;
        Ok(home.join(rest).to_string_lossy().into_owned())
    } else if path == "~" {
        let home = HOME
            .as_deref()
            .ok_or_else(|| tool_error("cannot expand ~: HOME not set"))?;
        Ok(home.to_string_lossy().into_owned())
    } else {
        Ok(path.to_string())
    }
}

/// Canonicalizes the longest existing prefix of `path`, keeping the missing
/// remainder as a tail so the result still compares against on-disk spellings.
fn canonicalize_prefix(path: &Path) -> PathBuf {
    let mut existing = path.to_path_buf();
    let mut tail = PathBuf::new();
    loop {
        if let Ok(canonical) = existing.canonicalize() {
            if tail.as_os_str().is_empty() {
                return canonical;
            }
            return canonical.join(&tail);
        }
        // Stop at the root (no file_name to pop) with the path verbatim.
        let Some(name) = existing.file_name().map(PathBuf::from) else {
            return path.to_path_buf();
        };
        tail = name.join(tail);
        existing.pop();
    }
}

/// Best-effort `.`/`..` normalization without touching the filesystem, for
/// tails `canonicalize` cannot resolve (it needs every component to exist).
fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                // Keep a leading `..` — nothing above the root to remove.
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Display form of a path: cwd-relative when inside it, `~/`-relative inside
/// home, verbatim otherwise.
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

/// `.git` is always excluded, even when `gitignore` is false.
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
    fn resolve_path_relative_anchors_to_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let tools = CodeTools::default().with_root(dir.path());
        let resolved = tools.resolve_path("Cargo.toml").unwrap();
        let root = dir.path().canonicalize().unwrap();
        assert_eq!(Path::new(&resolved), root.join("Cargo.toml"));
    }

    #[test]
    fn resolve_path_default_root_is_the_process_cwd() {
        let tools = CodeTools::default();
        let resolved = tools.resolve_path("Cargo.toml").unwrap();
        let cwd = env::current_dir().unwrap().canonicalize().unwrap();
        assert_eq!(Path::new(&resolved), cwd.join("Cargo.toml"));
    }

    #[test]
    fn resolve_path_rejects_absolute_paths_outside_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let tools = CodeTools::default().with_root(dir.path());
        let err = tools.resolve_path("/definitely/not/here.rs").unwrap_err();
        assert!(
            err.to_string().contains("escapes the workspace root"),
            "{err}"
        );
    }

    #[test]
    fn resolve_path_rejects_dot_dot_escape() {
        let dir = tempfile::tempdir().unwrap();
        let tools = CodeTools::default().with_root(dir.path());
        let escape = dir.path().join("../outside.rs");
        let err = tools.resolve_path(&escape.to_string_lossy()).unwrap_err();
        assert!(
            err.to_string().contains("escapes the workspace root"),
            "{err}"
        );
    }

    /// A symlink inside the root pointing outside it must not escape containment.
    #[test]
    #[cfg(unix)]
    fn resolve_path_rejects_symlink_escape() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();

        let tools = CodeTools::default().with_root(dir.path());
        let err = tools
            .resolve_path(&dir.path().join("link").to_string_lossy())
            .unwrap_err();
        assert!(
            err.to_string().contains("escapes the workspace root"),
            "{err}"
        );
    }

    #[test]
    fn resolve_path_missing_path_inside_the_root_still_resolves() {
        let dir = tempfile::tempdir().unwrap();
        let tools = CodeTools::default().with_root(dir.path());
        let missing = dir.path().join("missing.rs");
        let resolved = tools.resolve_path(&missing.to_string_lossy()).unwrap();
        assert_eq!(Path::new(&resolved), missing);
    }

    #[test]
    fn resolve_path_allows_outside_paths_when_opted_out() {
        let dir = tempfile::tempdir().unwrap();
        let tools = CodeTools::default()
            .with_root(dir.path())
            .with_external_paths(true);
        assert_eq!(
            tools.resolve_path("/definitely/not/here.rs").unwrap(),
            "/definitely/not/here.rs"
        );
    }

    #[test]
    fn repomap_cache_reuses_one_instance_per_root() {
        let tools = CodeTools::default();
        let dir = tempfile::tempdir().unwrap();
        let a = tools.repomap_for_root(dir.path(), None, false);
        let b = tools.repomap_for_root(dir.path(), None, false);
        // Same cached instance (Arc-shared caches).
        let text_a = a.get_repo_map(&[], &[], "query");
        let text_b = b.get_repo_map(&[], &[], "query");
        assert_eq!(text_a, text_b);
        // A custom budget must not poison the default-budget cache.
        let _custom = tools.repomap_for_root(dir.path(), Some(64), false);
        let c = tools.repomap_for_root(dir.path(), None, false);
        assert_eq!(c.get_repo_map(&[], &[], "query"), text_a);
    }
}
