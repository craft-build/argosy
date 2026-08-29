//! The `conflicts` tool: find and resolve git merge-conflict markers,
//! ported from Craft's `tools/conflicts.rs`.
//!
//! Port note: unlike Craft, which wrote resolutions with a bare
//! `std::fs::write`, the resolve path here goes through the same
//! read → stale-read check → write discipline as `astgrep` apply: every file
//! read is recorded, and a write is refused when the file changed since.

#[cfg(feature = "mcp")]
use rmcp::schemars;

use serde::{Deserialize, Serialize};

use super::{CodeTools, relative_path, resolve_path, tool_error};
use crate::error::Result;

const CONFLICT_START: &str = "<<<<<<< ";
const CONFLICT_SEPARATOR: &str = "=======";
const CONFLICT_END: &str = ">>>>>>> ";
const THEIRS: &str = "@theirs";
const OURS: &str = "@ours";
const BASE: &str = "@base";

/// `conflicts` parameters.
#[cfg_attr(feature = "mcp", derive(rmcp::schemars::JsonSchema))]
#[derive(Debug, Clone, Deserialize)]
pub struct ConflictsParams {
    /// Directory (or file) to scan (default: the server's working
    /// directory).
    pub path: Option<String>,
    /// Resolve conflicts instead of listing. `"@theirs"` keeps the incoming
    /// (their branch) side, `"@ours"` keeps the current (our branch) side,
    /// `"@base"` drops both sides. Omit to list only.
    pub resolve: Option<String>,
    /// Resolve only the Nth conflict (1-indexed) per file. Omit to resolve
    /// all conflicts in scope.
    pub index: Option<usize>,
}

/// The `conflicts` outcome.
#[derive(Debug, Clone, Serialize)]
pub struct ConflictsReport {
    /// The resolved scope path.
    pub path: String,
    /// The rendered listing (or resolution summary).
    pub text: String,
    /// Number of conflicts resolved; present only in resolve mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved: Option<usize>,
}

/// Runs the `conflicts` tool: list markers, or resolve them per `resolve`.
pub fn run(code: &CodeTools, params: ConflictsParams) -> Result<ConflictsReport> {
    let scope = params.path.as_deref().unwrap_or(".").to_string();
    let scope_path = resolve_path(&scope)?;

    if let Some(choice) = params.resolve.as_deref() {
        let side = match choice {
            THEIRS => ConflictSide::Theirs,
            OURS => ConflictSide::Ours,
            BASE => ConflictSide::Base,
            other => {
                return Err(tool_error(format!(
                    "unknown resolve choice \"{other}\"; use {THEIRS}, {OURS}, or {BASE}"
                )));
            }
        };
        let (resolved_files, total_conflicts, remaining) =
            resolve_in_scope(code, &scope_path, side, params.index);

        let text = if resolved_files.is_empty() {
            format!("no conflicts resolved ({total_conflicts} found, {remaining} remaining)")
        } else {
            let mut out = format!(
                "resolved {total_conflicts} conflict(s) as {choice} in {} file(s):\n",
                resolved_files.len()
            );
            for f in &resolved_files {
                out.push_str(&format!("  {f}\n"));
            }
            if remaining > 0 {
                out.push_str(&format!("{remaining} conflict(s) remain unresolved\n"));
            }
            out
        };
        return Ok(ConflictsReport {
            path: relative_path(&scope_path),
            text,
            resolved: Some(total_conflicts),
        });
    }

    let conflicts = collect_conflicts(code, &scope_path);

    let text = if conflicts.is_empty() {
        "no merge conflicts found".to_string()
    } else {
        let mut out = format!("merge conflicts in {} file(s):\n", conflicts.len());
        for (file, markers) in &conflicts {
            out.push_str(&format!("\n{file} ({} conflict(s)):\n", markers.len()));
            for m in markers {
                out.push_str(&format!(
                    "  {} - {}: {} vs {}\n",
                    m.start_line, m.end_line, m.our_branch, m.their_branch
                ));
            }
        }
        out
    };
    Ok(ConflictsReport {
        path: relative_path(&scope_path),
        text,
        resolved: None,
    })
}

#[derive(Debug, Clone)]
pub(crate) struct ConflictMarker {
    pub(crate) start_line: usize,
    pub(crate) end_line: usize,
    pub(crate) our_branch: String,
    pub(crate) their_branch: String,
}

fn parse_conflicts(content: &str) -> Vec<ConflictMarker> {
    let mut markers = Vec::new();
    let mut current: Option<ConflictMarker> = None;

    for (i, line) in content.lines().enumerate() {
        if let Some(branch) = line.strip_prefix(CONFLICT_START) {
            current = Some(ConflictMarker {
                start_line: i + 1,
                end_line: 0,
                our_branch: branch.trim().to_string(),
                their_branch: String::new(),
            });
        } else if line == CONFLICT_SEPARATOR && current.is_some() {
        } else if let Some(branch) = line.strip_prefix(CONFLICT_END)
            && let Some(mut m) = current.take()
        {
            m.end_line = i + 1;
            m.their_branch = branch.trim().to_string();
            markers.push(m);
        }
    }

    markers
}

#[derive(Clone, Copy)]
enum ConflictSide {
    Ours,
    Theirs,
    Base,
}

/// Rewrite a file's content, resolving conflict markers according to `side`.
/// `index` selects only the Nth (1-indexed) conflict; `None` resolves all.
/// Returns the new content and the number of conflicts resolved.
fn resolve_content(content: &str, side: ConflictSide, index: Option<usize>) -> (String, usize) {
    let mut out = String::with_capacity(content.len());
    let mut state = 0u8;
    let mut ours = String::new();
    let mut theirs = String::new();
    let mut count = 0usize;
    let mut resolved = 0usize;
    let want = index.is_some();
    let want_n = index.unwrap_or(0);

    for line in content.split_inclusive('\n') {
        let bare = line.strip_suffix('\n').unwrap_or(line);
        match state {
            0 => {
                if bare.starts_with(CONFLICT_START) {
                    count += 1;
                    state = 1;
                    ours.clear();
                    theirs.clear();
                } else {
                    out.push_str(line);
                }
            }
            1 => {
                if bare == CONFLICT_SEPARATOR {
                    state = 2;
                } else {
                    ours.push_str(line);
                }
            }
            _ => {
                if let Some(_branch) = bare.strip_prefix(CONFLICT_END) {
                    let target = if want && want_n != count {
                        None
                    } else {
                        Some(match side {
                            ConflictSide::Ours => ours.as_str(),
                            ConflictSide::Theirs => theirs.as_str(),
                            ConflictSide::Base => "",
                        })
                    };
                    match target {
                        Some(t) => {
                            out.push_str(t);
                            resolved += 1;
                        }
                        None => {
                            out.push_str(CONFLICT_START);
                            out.push_str(" ours\n");
                            out.push_str(&ours);
                            out.push_str(CONFLICT_SEPARATOR);
                            out.push('\n');
                            out.push_str(&theirs);
                            out.push_str(CONFLICT_END);
                            out.push_str(" theirs\n");
                        }
                    }
                    state = 0;
                } else {
                    theirs.push_str(line);
                }
            }
        }
    }
    let _ = want;
    (out, resolved)
}

fn resolve_in_scope(
    code: &CodeTools,
    scope_path: &str,
    side: ConflictSide,
    index: Option<usize>,
) -> (Vec<String>, usize, usize) {
    let files = collect_conflict_files(scope_path);
    let mut resolved_files = Vec::new();
    let mut total_resolved = 0usize;
    let mut remaining = 0usize;

    for (path, content) in files {
        let (new_content, resolved) = resolve_content(&content, side, index);
        let marker_count = parse_conflicts(&content).len();
        remaining += marker_count.saturating_sub(resolved);
        if resolved > 0 {
            // The stale-read guard compares against the last read from an
            // earlier tool call (zoom, ...); this scan deliberately does not
            // record its reads first, so the guard stays meaningful.
            if let Err(message) = code.check_before_edit(std::path::Path::new(&path)) {
                tracing::warn!(
                    path = %path,
                    %message,
                    "conflicts resolve: skipping file"
                );
                remaining += resolved;
                continue;
            }
            if let Err(e) = std::fs::write(&path, &new_content) {
                tracing::warn!(path = %path, error = %e, "conflicts resolve: write failed");
                remaining += resolved;
                continue;
            }
            code.record_read(std::path::Path::new(&path));
            total_resolved += resolved;
            resolved_files.push(relative_path(&path));
        }
    }
    (resolved_files, total_resolved, remaining)
}

fn collect_conflict_files(scope_path: &str) -> Vec<(String, String)> {
    let builder = ignore::WalkBuilder::new(scope_path)
        .hidden(true)
        .git_ignore(true)
        .build();
    let mut out = Vec::new();
    for entry in builder.flatten() {
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let path = entry.path();
        if let Ok(content) = std::fs::read_to_string(path)
            && content.contains(CONFLICT_START)
        {
            out.push((path.to_string_lossy().into_owned(), content));
        }
    }
    out
}

fn collect_conflicts(code: &CodeTools, scope_path: &str) -> Vec<(String, Vec<ConflictMarker>)> {
    let builder = ignore::WalkBuilder::new(scope_path)
        .hidden(true)
        .git_ignore(true)
        .build();

    let mut conflicts = Vec::new();
    for entry in builder.flatten() {
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let path = entry.path();
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        code.record_read(path);
        let markers = parse_conflicts(&content);
        if !markers.is_empty() {
            let rel = relative_path(&path.to_string_lossy());
            conflicts.push((rel, markers));
        }
    }
    conflicts
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    #[test]
    fn parse_conflicts_finds_single() {
        let content = "\
some code
<<<<<<< HEAD
our change
=======
their change
>>>>>>> feature
more code";
        let markers = parse_conflicts(content);
        assert_eq!(markers.len(), 1);
        assert_eq!(markers[0].start_line, 2);
        assert_eq!(markers[0].end_line, 6);
        assert_eq!(markers[0].our_branch, "HEAD");
        assert_eq!(markers[0].their_branch, "feature");
    }

    #[test]
    fn parse_conflicts_finds_multiple() {
        let content = "\
<<<<<<< a
x
=======
y
>>>>>>> b
code
<<<<<<< c
p
=======
q
>>>>>>> d";
        let markers = parse_conflicts(content);
        assert_eq!(markers.len(), 2);
    }

    #[test]
    fn parse_conflicts_no_markers() {
        let content = "clean file\nno conflicts\n";
        let markers = parse_conflicts(content);
        assert!(markers.is_empty());
    }

    const CONFLICT_TEXT: &str = "\
top
<<<<<<< HEAD
ours-line
=======
theirs-line
>>>>>>> feature
bottom
<<<<<<< HEAD
second-ours
=======
second-theirs
>>>>>>> other
end";

    #[test_case(ConflictSide::Ours, "top\nours-line\nbottom\nsecond-ours\nend" ; "resolve_all_ours")]
    #[test_case(ConflictSide::Theirs, "top\ntheirs-line\nbottom\nsecond-theirs\nend" ; "resolve_all_theirs")]
    #[test_case(ConflictSide::Base, "top\nbottom\nend" ; "resolve_all_base")]
    fn resolve_content_all(side: ConflictSide, expected: &str) {
        let (out, count) = resolve_content(CONFLICT_TEXT, side, None);
        assert_eq!(count, 2);
        assert_eq!(out, expected);
    }

    #[test]
    fn resolve_content_only_nth_keeps_others() {
        let (out, count) = resolve_content(CONFLICT_TEXT, ConflictSide::Theirs, Some(2));
        assert_eq!(count, 1);
        assert!(
            out.contains("ours-line"),
            "first conflict should be untouched"
        );
        assert!(out.contains("second-theirs"));
    }

    #[test]
    fn run_lists_conflicts() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), CONFLICT_TEXT).unwrap();

        let tools = CodeTools::default();
        let report = run(
            &tools,
            ConflictsParams {
                path: Some(dir.path().to_string_lossy().into_owned()),
                resolve: None,
                index: None,
            },
        )
        .unwrap();
        assert!(report.text.contains("merge conflicts in 1 file(s)"));
        assert!(report.text.contains("HEAD vs feature"));
        assert!(report.resolved.is_none());
    }

    #[test]
    fn run_resolves_theirs() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f.txt");
        std::fs::write(&file, CONFLICT_TEXT).unwrap();

        let tools = CodeTools::default();
        let report = run(
            &tools,
            ConflictsParams {
                path: Some(dir.path().to_string_lossy().into_owned()),
                resolve: Some("@theirs".into()),
                index: None,
            },
        )
        .unwrap();
        assert_eq!(report.resolved, Some(2));
        let after = std::fs::read_to_string(&file).unwrap();
        assert_eq!(after, "top\ntheirs-line\nbottom\nsecond-theirs\nend");
    }

    #[test]
    fn run_resolves_only_nth() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f.txt");
        std::fs::write(&file, CONFLICT_TEXT).unwrap();

        let tools = CodeTools::default();
        let report = run(
            &tools,
            ConflictsParams {
                path: Some(dir.path().to_string_lossy().into_owned()),
                resolve: Some("@ours".into()),
                index: Some(2),
            },
        )
        .unwrap();
        assert_eq!(report.resolved, Some(1));
        let after = std::fs::read_to_string(&file).unwrap();
        assert!(after.contains("theirs-line"), "got {after}");
        assert!(after.contains("second-ours"), "got {after}");
    }

    #[test]
    fn run_rejects_unknown_choice() {
        let tools = CodeTools::default();
        let err = run(
            &tools,
            ConflictsParams {
                path: None,
                resolve: Some("@nope".into()),
                index: None,
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("unknown resolve choice"), "{err}");
    }
}
