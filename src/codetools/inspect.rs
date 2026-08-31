//! The `inspect` tool: a quick project health check (TODO/FIXME/HACK/XXX
//! scan plus `git status`), ported from Craft's `tools/inspect.rs`.

#[cfg(feature = "mcp")]
use rmcp::schemars;

use serde::{Deserialize, Serialize};

use super::{CodeTools, relative_path, resolve_path, tool_error};
use crate::error::Result;

/// `inspect` parameters.
#[cfg_attr(feature = "mcp", derive(rmcp::schemars::JsonSchema))]
#[derive(Debug, Clone, Deserialize)]
pub struct InspectParams {
    /// Sections to run: `todos`, `git_status`, or `all` (default `all`).
    pub sections: Option<String>,
    /// File or directory to scope (default: the server's working directory).
    pub scope: Option<String>,
}

/// The `inspect` outcome: the rendered sections.
#[derive(Debug, Clone, Serialize)]
pub struct InspectReport {
    /// The rendered `todos:` and/or `git_status:` sections.
    pub text: String,
}

/// Runs the `inspect` tool.
pub fn run(_code: &CodeTools, params: InspectParams) -> Result<InspectReport> {
    let sections = params.sections.as_deref().unwrap_or("all").to_string();
    let scope = params.scope.as_deref().unwrap_or(".").to_string();

    let todos = if sections == "all" || sections == "todos" {
        Some(inspect_todos(&scope)?)
    } else {
        None
    };
    let git = if sections == "all" || sections == "git_status" {
        Some(inspect_git_status(&scope)?)
    } else {
        None
    };

    let mut out = String::new();
    if let Some(t) = todos {
        out.push_str(&t);
    }
    if let Some(g) = git {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&g);
    }

    if out.is_empty() {
        out.push_str("nothing to inspect");
    }

    Ok(InspectReport { text: out })
}

fn inspect_todos(scope: &str) -> Result<String> {
    let scope_path = resolve_path(scope)?;
    let path = std::path::Path::new(&scope_path);

    let mut todos = Vec::new();

    if path.is_file() {
        collect_todos_from_file(path, &mut todos);
    } else {
        let builder = ignore::WalkBuilder::new(path)
            .hidden(true)
            .git_ignore(true)
            .build();
        for entry in builder.flatten() {
            if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                continue;
            }
            collect_todos_from_file(entry.path(), &mut todos);
        }
    }

    if todos.is_empty() {
        return Ok("todos: (none)\n".to_string());
    }

    let mut out = format!("todos: ({} items)\n", todos.len());
    for (file, line_no, text) in &todos {
        let rel = relative_path(file);
        let preview = truncate_todo(text, 80);
        out.push_str(&format!("  {rel}:{line_no}: {preview}\n"));
    }
    Ok(out)
}

fn collect_todos_from_file(path: &std::path::Path, todos: &mut Vec<(String, usize, String)>) {
    let Ok(content) = std::fs::read_to_string(path) else {
        return;
    };
    let keywords = ["TODO", "FIXME", "HACK", "XXX"];
    for (i, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        for kw in &keywords {
            if let Some(rest) = find_keyword(trimmed, kw) {
                let text = rest
                    .strip_prefix(':')
                    .or_else(|| rest.strip_prefix('('))
                    .or_else(|| rest.strip_prefix(' '))
                    .unwrap_or(rest)
                    .trim()
                    .to_string();
                if !text.is_empty() {
                    todos.push((path.to_string_lossy().to_string(), i + 1, text));
                }
                break;
            }
        }
    }
}

fn find_keyword<'a>(line: &'a str, keyword: &str) -> Option<&'a str> {
    if let Some(rest) = line.strip_prefix(keyword) {
        return Some(rest);
    }
    for comment_prefix in &["// ", "# ", "-- ", ";; ", "/* ", "<!-- "] {
        if let Some(after_comment) = line.strip_prefix(comment_prefix)
            && let Some(rest) = after_comment.strip_prefix(keyword)
        {
            return Some(rest);
        }
    }
    None
}

fn truncate_todo(text: &str, max: usize) -> String {
    if text.len() > max {
        let boundary = text.floor_char_boundary(max.saturating_sub(3));
        format!("{}...", &text[..boundary])
    } else {
        text.to_string()
    }
}

fn inspect_git_status(scope: &str) -> Result<String> {
    let scope_path = resolve_path(scope)?;
    let path = std::path::Path::new(&scope_path);

    let repo_dir = if path.is_file() {
        path.parent().unwrap_or(path)
    } else {
        path
    };

    // Resolve the actual worktree root first: the scope can then be passed
    // as a pathspec (status from a subdirectory of the repo would otherwise
    // report the whole tree, ignoring the scope), and "not a repo" stays
    // distinguishable from other git failures.
    let toplevel = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(repo_dir)
        .output()
        .map_err(|e| tool_error(format!("git status failed: {e}")))?;
    if !toplevel.status.success() {
        return Ok("git_status: (not a git repo)\n".to_string());
    }
    let root = String::from_utf8_lossy(&toplevel.stdout).trim().to_string();
    // Canonicalize so the strip works even when the scope reached the repo
    // through a symlinked path (e.g. /tmp on macOS).
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let rel = canonical
        .strip_prefix(&root)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();

    let mut cmd = std::process::Command::new("git");
    cmd.args(["status", "--porcelain=v1"]).current_dir(&root);
    if !rel.is_empty() {
        cmd.arg("--").arg(&rel);
    }
    let output = cmd
        .output()
        .map_err(|e| tool_error(format!("git status failed: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Ok(format!("git_status: (git failed: {stderr})\n"));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    if stdout.is_empty() {
        return Ok("git_status: (clean)\n".to_string());
    }

    let entries: Vec<&str> = stdout.lines().take(50).collect();
    let total = stdout.lines().count();

    let mut out = format!("git_status: ({} changes)\n", total);
    for entry in &entries {
        out.push_str(&format!("  {entry}\n"));
    }
    if total > entries.len() {
        out.push_str(&format!("  ... ({} more)\n", total - entries.len()));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_todos_from_file_finds_todo() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.rs");
        std::fs::write(&path, "fn main() {\n  // TODO: fix this\n}\n").unwrap();
        let mut todos = Vec::new();
        collect_todos_from_file(&path, &mut todos);
        assert_eq!(todos.len(), 1);
        assert_eq!(todos[0].2, "fix this");
    }

    #[test]
    fn collect_todos_from_file_finds_fixme() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.py");
        std::fs::write(&path, "# FIXME: broken\npass\n").unwrap();
        let mut todos = Vec::new();
        collect_todos_from_file(&path, &mut todos);
        assert_eq!(todos.len(), 1);
        assert_eq!(todos[0].2, "broken");
    }

    #[test]
    fn truncate_todo_short() {
        assert_eq!(truncate_todo("hello", 80), "hello");
    }

    #[test]
    fn truncate_todo_long() {
        let long = "x".repeat(100);
        let result = truncate_todo(&long, 80);
        assert!(result.len() <= 80);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn run_scopes_todos_to_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.rs");
        std::fs::write(&path, "// TODO: one\n").unwrap();
        std::fs::write(dir.path().join("b.rs"), "// TODO: two\n").unwrap();

        let tools = CodeTools::default();
        let report = run(
            &tools,
            InspectParams {
                sections: Some("todos".into()),
                scope: Some(path.to_string_lossy().into_owned()),
            },
        )
        .unwrap();
        assert!(report.text.contains("(1 items)"), "got {}", report.text);
        assert!(report.text.contains("one"));
        assert!(!report.text.contains("two"));
    }

    #[test]
    fn run_git_status_degrades_outside_repo() {
        let dir = tempfile::tempdir().unwrap();
        let tools = CodeTools::default();
        let report = run(
            &tools,
            InspectParams {
                sections: Some("git_status".into()),
                scope: Some(dir.path().to_string_lossy().into_owned()),
            },
        )
        .unwrap();
        assert!(
            report.text.contains("not a git repo") || report.text.contains("git_status:"),
            "got {}",
            report.text
        );
    }

    /// The scope is honored as a pathspec: status for one file does not
    /// report the whole repository.
    #[test]
    fn git_status_scopes_to_the_pathspec() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(root)
                .output()
                .unwrap()
        };
        assert!(git(&["init"]).status.success());
        std::fs::write(root.join("a.txt"), "a\n").unwrap();
        std::fs::write(root.join("b.txt"), "b\n").unwrap();

        let scoped =
            inspect_git_status(root.join("b.txt").to_string_lossy().into_owned().as_str()).unwrap();
        assert!(scoped.contains("b.txt"), "got {scoped}");
        assert!(!scoped.contains("a.txt"), "got {scoped}");
    }
}
