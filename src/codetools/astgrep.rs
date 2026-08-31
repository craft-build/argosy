//! The `astgrep` tool: AST search and replace with metavariables, ported
//! from Craft's `tools/astgrep.rs`.
//!
//! Port notes: Craft compiled only four grammars (rust, typescript/tsx,
//! python, go) while claiming ~22 languages; argosy enables ast-grep's full
//! `builtin-parser` set, so every name `parse_lang` accepts genuinely works,
//! and the error message lists exactly those. The write path keeps Craft's
//! discipline: dry-run diffs by default, `apply` writes only after a
//! stale-read check and a syntax re-validation that rolls back bad
//! replacements.

use std::path::PathBuf;

use ast_grep_core::NodeMatch;
use ast_grep_language::{LanguageExt, SupportLang};
use serde::{Deserialize, Serialize};
use similar::ChangeTag;

#[cfg(feature = "mcp")]
use rmcp::schemars;

use super::{CodeTools, relative_path, resolve_path, tool_error, walk_builder_opts};
use crate::error::Result;

/// `astgrep` parameters.
#[cfg_attr(feature = "mcp", derive(rmcp::schemars::JsonSchema))]
#[derive(Debug, Clone, Deserialize)]
pub struct AstgrepParams {
    /// AST pattern with `$VAR` (single node) and `$$$BODY` (zero or more
    /// nodes) metavariables.
    pub pattern: String,
    /// Replacement pattern; omit for search-only mode. Uses `$VAR` refs
    /// from the pattern.
    pub rewrite: Option<String>,
    /// Language name (see the tool description for the list).
    pub lang: String,
    /// Directory or file to search (default: the server's working
    /// directory).
    pub path: Option<String>,
    /// Glob patterns to include (e.g. `["*.rs", "src/**"]`).
    pub globs: Option<Vec<String>>,
    /// Apply the replacement (default: dry-run, diffs only).
    pub apply: Option<bool>,
}

/// The `astgrep` outcome.
#[derive(Debug, Clone, Serialize)]
pub struct AstgrepReport {
    /// `"search"`, `"diff"` (dry-run replace), or `"apply"`.
    pub mode: &'static str,
    /// Header (mode, pattern, match counts) plus match previews or unified
    /// diffs, capped at 30 KB.
    pub text: String,
    /// Total matches across files; search mode only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matches: Option<usize>,
    /// Files successfully written; apply mode only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files_changed: Option<usize>,
    /// True iff at least one file's replacement was rolled back for
    /// introducing syntax errors; apply mode only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rolled_back: Option<bool>,
}

/// Runs the `astgrep` tool: search, dry-run replace, or applied replace.
pub fn run(code: &CodeTools, params: AstgrepParams) -> Result<AstgrepReport> {
    let lang = parse_lang(&params.lang)?;
    let is_replace = params.rewrite.is_some();
    let apply = params.apply.unwrap_or(false) && is_replace;
    let pattern_str = params.pattern.as_str();
    let rewrite_str = params.rewrite.as_deref();

    // Mutating runs serialize against each other (see `CodeTools::begin_write`)
    // so the stale-read check below cannot race a concurrent apply's write.
    let _write_guard = if apply {
        Some(code.begin_write())
    } else {
        None
    };

    let search_path = resolve_path(params.path.as_deref().unwrap_or("."))?;
    let globs = params.globs.clone().unwrap_or_default();
    let lang_types = lang.file_types();

    let glob_refs: Vec<&str> = globs.iter().map(|s| s.as_str()).collect();
    let mut builder = walk_builder_opts(&search_path, &glob_refs, true)?;
    builder.types(lang_types);
    let paths: Vec<PathBuf> = builder
        .build()
        .flatten()
        .filter(|entry| entry.file_type().is_some_and(|ft| ft.is_file()))
        .map(|entry| entry.into_path())
        .collect();

    let mut results = Vec::new();
    let mut files_scanned = 0u64;
    let mut files_matched = 0u64;
    let mut files_changed = 0usize;
    let mut rolled_back = false;
    let mut total_matches = 0usize;

    for path in paths {
        // The stale-read guard runs BEFORE this call's own read+record, so
        // it compares against the last read from an earlier tool call
        // (zoom, a previous astgrep, ...). Craft checked after recording,
        // which made its own check a no-op; checking first implements the
        // documented intent. A stale file skips that file's apply only —
        // rewrites already written to other files must be reported, not
        // buried by one opaque error — and its read stays unrecorded so
        // the guard keeps comparing against the earlier, now-stale view.
        let stale = if apply {
            code.check_before_edit(&path).err()
        } else {
            None
        };
        if let Some(message) = stale {
            let rel = relative_path(&path.to_string_lossy());
            results.push(format!("{rel}: SKIPPED — {message}"));
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        code.record_read(&path);

        files_scanned += 1;
        let grep = lang.ast_grep(&content);
        let matches: Vec<NodeMatch<_>> = grep.root().find_all(pattern_str).collect();

        if matches.is_empty() {
            continue;
        }

        files_matched += 1;

        if is_replace {
            let rw = rewrite_str.expect("is_replace implies rewrite is Some");
            let mut grep2 = lang.ast_grep(&content);
            let edits = grep2.root().replace_all(pattern_str, rw);
            for edit in edits.into_iter().rev() {
                grep2.edit(edit).map_err(|e| tool_error(e.to_string()))?;
            }
            let new_content = grep2.generate();
            let rel = relative_path(&path.to_string_lossy());

            if apply {
                // Roll back only when the replacement INTRODUCES errors: a
                // file that already carried error nodes (WIP code) must not
                // have every rewrite rejected for damage it did not cause.
                let repl_grep = lang.ast_grep(&new_content);
                if has_error_or_missing(&repl_grep.root())
                    && !has_error_or_missing(&grep.root())
                {
                    rolled_back = true;
                    results.push(format!(
                        "{rel}: ROLLED BACK — replacement introduces syntax errors"
                    ));
                    continue;
                }
                let diff_count = count_changes(&content, &new_content);
                // A per-file write failure skips that file only; the rest
                // of the run (and its report) must survive it.
                if let Err(e) = std::fs::write(&path, &new_content) {
                    results.push(format!("{rel}: SKIPPED — write error: {e}"));
                    continue;
                }
                code.record_read(&path);
                files_changed += 1;
                results.push(format!("{rel}: {diff_count} replacement(s) applied"));
            } else {
                let diff = unified_diff(&content, &new_content, &rel);
                if !diff.is_empty() {
                    results.push(diff);
                }
            }
        } else {
            total_matches += matches.len();
            let rel = relative_path(&path.to_string_lossy());
            for m in &matches {
                let pos = m.start_pos();
                let text = m.text();
                let preview = truncate_match(&text, 200);
                results.push(format!("{rel}:{line}: {preview}", line = pos.line() + 1));
            }
        }
    }

    if results.is_empty() {
        return Ok(AstgrepReport {
            mode: if is_replace {
                if apply { "apply" } else { "diff" }
            } else {
                "search"
            },
            text: format!(
                "no matches for \"{}\" in {search_path} ({files_scanned} files scanned)",
                params.pattern
            ),
            matches: if is_replace { None } else { Some(0) },
            files_changed: if apply { Some(0) } else { None },
            rolled_back: if apply { Some(false) } else { None },
        });
    }

    let mode = if is_replace {
        if apply { "apply" } else { "diff" }
    } else {
        "search"
    };
    let header = format!(
        "{mode}: \"{pattern}\" [{lang}] in {search_path}\n{files_matched}/{files_scanned} files matched\n",
        pattern = params.pattern,
        lang = params.lang,
    );
    let body = truncate_results(&results, 30_000);
    Ok(AstgrepReport {
        mode,
        text: header + &body,
        matches: if is_replace {
            None
        } else {
            Some(total_matches)
        },
        files_changed: if apply { Some(files_changed) } else { None },
        rolled_back: if apply { Some(rolled_back) } else { None },
    })
}

pub(crate) fn parse_lang(s: &str) -> Result<SupportLang> {
    s.parse::<SupportLang>()
        .map_err(|_| tool_error(format!("unsupported language \"{s}\"; use one of: bash, c, cpp, csharp, css, dart, elixir, go, haskell, hcl, html, java, javascript, json, kotlin, lua, markdown, nix, php, python, ruby, rust, scala, solidity, swift, tsx, typescript, yaml")))
}

pub(crate) fn has_error_or_missing<D: ast_grep_core::Doc>(node: &ast_grep_core::Node<D>) -> bool {
    if node.is_error() || node.is_missing() {
        return true;
    }
    node.dfs().any(|n| n.is_error() || n.is_missing())
}

pub(crate) fn count_changes(old: &str, new: &str) -> usize {
    let diff = similar::TextDiff::from_lines(old, new);
    diff.iter_all_changes()
        .filter(|c| c.tag() == ChangeTag::Delete || c.tag() == ChangeTag::Insert)
        .count()
        .div_ceil(2)
        .max(1)
}

fn truncate_match(text: &str, max: usize) -> String {
    let first_line = text.lines().next().unwrap_or(text);
    if first_line.len() > max {
        let boundary = first_line.floor_char_boundary(max.saturating_sub(3));
        format!("{}...", &first_line[..boundary])
    } else if text.lines().count() > 1 {
        format!("{first_line} ...")
    } else {
        first_line.to_string()
    }
}

fn truncate_results(results: &[String], max_bytes: usize) -> String {
    let mut out = String::new();
    for (i, r) in results.iter().enumerate() {
        if out.len() + r.len() + 1 > max_bytes {
            out.push_str(&format!(
                "\n... ({} more results truncated)",
                results.len() - i
            ));
            break;
        }
        out.push_str(r);
        out.push('\n');
    }
    out
}

pub(crate) fn unified_diff(old: &str, new: &str, path: &str) -> String {
    let diff = similar::TextDiff::from_lines(old, new);
    let mut out = String::new();
    for hunk in diff
        .unified_diff()
        .header(&format!("--- {path}"), &format!("+++ {path}"))
        .iter_hunks()
    {
        let _ = std::fmt::write(&mut out, format_args!("{hunk}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_lang_rust() {
        assert!(parse_lang("rust").is_ok());
        assert!(parse_lang("CSharp").is_ok(), "aliases are case-insensitive");
    }

    #[test]
    fn parse_lang_invalid() {
        assert!(parse_lang("brainfuck").is_err());
    }

    #[test]
    fn parse_lang_error_lists_all_supported() {
        let err = parse_lang("nope").unwrap_err().to_string();
        for name in [
            "rust",
            "typescript",
            "tsx",
            "python",
            "go",
            "java",
            "haskell",
            "solidity",
            "yaml",
        ] {
            assert!(err.contains(name), "{name} missing from: {err}");
        }
        // Craft's message promised languages it never compiled; ours must not.
        assert!(!err.contains("starlark"), "{err}");
        assert!(!err.contains("zig"), "{err}");
    }

    #[test]
    fn truncate_match_short() {
        assert_eq!(truncate_match("fn foo() {}", 200), "fn foo() {}");
    }

    #[test]
    fn truncate_match_multiline() {
        assert_eq!(
            truncate_match("fn foo() {\n  body\n}", 200),
            "fn foo() { ..."
        );
    }

    #[test]
    fn truncate_match_multibyte_safe() {
        let s = "界".repeat(100);
        let result = truncate_match(&s, 10);
        assert!(result.ends_with("..."));
        assert!(result.chars().count() < 100);
    }

    #[test]
    fn count_changes_counts_replacements() {
        let old = "hello\nworld";
        let new = "hello\nearth";
        assert_eq!(count_changes(old, new), 1);
    }

    #[test]
    fn has_error_or_missing_rejects_invalid() {
        let grep = SupportLang::Rust.ast_grep("fn valid() { struct }");
        assert!(has_error_or_missing(&grep.root()));
    }

    #[test]
    fn has_error_or_missing_accepts_valid() {
        let grep = SupportLang::Rust.ast_grep("fn valid() {}");
        assert!(!has_error_or_missing(&grep.root()));
    }

    #[test]
    fn has_error_or_missing_detects_missing_node() {
        let grep = SupportLang::Rust.ast_grep("fn valid() {");
        assert!(has_error_or_missing(&grep.root()));
    }

    #[test]
    fn replace_all_applies_edits() {
        let mut grep = SupportLang::Rust.ast_grep("Vec::new(); Vec::new();");
        let edits = grep.root().replace_all("Vec::new()", "vec![]");
        for edit in edits.into_iter().rev() {
            grep.edit(edit).unwrap();
        }
        assert_eq!(grep.generate(), "vec![]; vec![];");
    }

    #[test]
    fn replace_all_preserves_metavar() {
        let mut grep = SupportLang::Rust.ast_grep("foo(1); foo(2);");
        let edits = grep.root().replace_all("foo($X)", "bar($X)");
        for edit in edits.into_iter().rev() {
            grep.edit(edit).unwrap();
        }
        assert_eq!(grep.generate(), "bar(1); bar(2);");
    }

    fn write_fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/main.rs"),
            "fn main() {\n    println!(\"hi\");\n}\n",
        )
        .unwrap();
        dir
    }

    fn params(dir: &tempfile::TempDir, rewrite: Option<&str>, apply: bool) -> AstgrepParams {
        AstgrepParams {
            pattern: "println!($MSG)".into(),
            rewrite: rewrite.map(str::to_string),
            lang: "rust".into(),
            path: Some(dir.path().to_string_lossy().into_owned()),
            globs: None,
            apply: Some(apply),
        }
    }

    #[test]
    fn run_search_finds_matches() {
        let dir = write_fixture();
        let tools = CodeTools::default();
        let report = run(&tools, params(&dir, None, false)).unwrap();
        assert_eq!(report.mode, "search");
        assert_eq!(report.matches, Some(1));
        assert!(report.text.contains("src/main.rs:2"), "got {}", report.text);
        assert!(report.text.contains("println"));
    }

    #[test]
    fn run_dry_run_leaves_file_untouched() {
        let dir = write_fixture();
        let file = dir.path().join("src/main.rs");
        let before = std::fs::read_to_string(&file).unwrap();

        let tools = CodeTools::default();
        let report = run(&tools, params(&dir, Some("eprintln!($MSG)"), false)).unwrap();
        assert_eq!(report.mode, "diff");
        assert!(
            report.text.contains("@@ -1,3 +1,3 @@"),
            "got {}",
            report.text
        );
        assert!(
            report.text.contains("-    println!(\"hi\");"),
            "got {}",
            report.text
        );
        assert!(
            report.text.contains("+    eprintln!(\"hi\");"),
            "got {}",
            report.text
        );
        assert_eq!(std::fs::read_to_string(&file).unwrap(), before);
    }

    #[test]
    fn run_apply_writes_and_reports() {
        let dir = write_fixture();
        let file = dir.path().join("src/main.rs");

        let tools = CodeTools::default();
        let report = run(&tools, params(&dir, Some("eprintln!($MSG)"), true)).unwrap();
        assert_eq!(report.mode, "apply");
        assert_eq!(report.files_changed, Some(1));
        assert_eq!(report.rolled_back, Some(false));
        let after = std::fs::read_to_string(&file).unwrap();
        assert!(after.contains("eprintln!"), "got {after}");
    }

    #[test]
    fn run_apply_rolls_back_syntax_errors() {
        let dir = write_fixture();
        let file = dir.path().join("src/main.rs");
        let before = std::fs::read_to_string(&file).unwrap();

        let tools = CodeTools::default();
        let report = run(&tools, params(&dir, Some(")))) not rust"), true)).unwrap();
        assert_eq!(report.rolled_back, Some(true));
        // The file must be untouched.
        assert_eq!(std::fs::read_to_string(&file).unwrap(), before);
    }

    /// The rollback guard blames the replacement only: a file that already
    /// carried error nodes (WIP code) must still accept valid rewrites.
    #[test]
    fn run_apply_tolerates_pre_existing_syntax_errors() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/main.rs"),
            "fn broken() { struct }\nfn main() {\n    println!(\"hi\");\n}\n",
        )
        .unwrap();

        let tools = CodeTools::default();
        let report = run(&tools, params(&dir, Some("eprintln!($MSG)"), true)).unwrap();
        assert_eq!(report.files_changed, Some(1));
        assert_eq!(report.rolled_back, Some(false));
        assert!(
            std::fs::read_to_string(dir.path().join("src/main.rs"))
                .unwrap()
                .contains("eprintln!")
        );
    }

    #[test]
    fn run_apply_skips_stale_read() {
        let dir = write_fixture();
        let file = dir.path().join("src/main.rs");

        let tools = CodeTools::default();
        // First call records the read (dry-run), then the file changes
        // externally before the apply.
        run(&tools, params(&dir, Some("eprintln!($MSG)"), false)).unwrap();
        let changed = "fn main() {\n    println!(\"changed\");\n}\n";
        std::fs::write(&file, changed).unwrap();

        // The stale file is skipped and reported, not a whole-run error.
        let report = run(&tools, params(&dir, Some("eprintln!($MSG)"), true)).unwrap();
        assert_eq!(report.files_changed, Some(0));
        assert!(
            report.text.contains("changed since last read"),
            "got {}",
            report.text
        );
        assert_eq!(std::fs::read_to_string(&file).unwrap(), changed);
    }

    /// One stale file must not bury rewrites already written to others:
    /// apply is per-file, and the report says exactly which file skipped.
    #[test]
    fn run_apply_skips_stale_file_and_applies_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/a.rs"), "fn a() { println!(\"a\"); }\n").unwrap();
        std::fs::write(dir.path().join("src/b.rs"), "fn b() { println!(\"b\"); }\n").unwrap();

        let tools = CodeTools::default();
        run(&tools, params(&dir, Some("eprintln!($MSG)"), false)).unwrap();
        let changed = "fn b() { println!(\"changed\"); }\n";
        std::fs::write(dir.path().join("src/b.rs"), changed).unwrap();

        let report = run(&tools, params(&dir, Some("eprintln!($MSG)"), true)).unwrap();
        assert_eq!(report.files_changed, Some(1));
        assert!(
            report.text.contains("src/b.rs") && report.text.contains("SKIPPED"),
            "got {}",
            report.text
        );
        assert!(
            std::fs::read_to_string(dir.path().join("src/a.rs"))
                .unwrap()
                .contains("eprintln!")
        );
        assert_eq!(std::fs::read_to_string(dir.path().join("src/b.rs")).unwrap(), changed);
    }

    #[test]
    fn run_no_matches() {
        let dir = write_fixture();
        let tools = CodeTools::default();
        let mut p = params(&dir, None, false);
        p.pattern = "nonexistent_function($$$A)".into();
        let report = run(&tools, p).unwrap();
        assert!(report.text.contains("no matches"), "got {}", report.text);
        assert_eq!(report.matches, Some(0));
    }

    #[test]
    fn run_rejects_unknown_lang() {
        let tools = CodeTools::default();
        let mut p = params(&write_fixture(), None, false);
        p.lang = "starlark".into();
        let err = run(&tools, p).unwrap_err();
        assert!(err.to_string().contains("unsupported language"), "{err}");
    }
}
