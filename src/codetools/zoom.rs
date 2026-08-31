//! The `zoom` tool: the body of one symbol (or a line range) in a file,
//! ported from Craft's `tools/zoom.rs`.

use std::path::Path;

#[cfg(feature = "mcp")]
use rmcp::schemars;

use serde::{Deserialize, Serialize};

use super::outline::{self, LangId};
use super::{CodeTools, relative_path, resolve_path, tool_error};
use crate::error::Result;

/// `zoom` parameters.
#[cfg_attr(feature = "mcp", derive(rmcp::schemars::JsonSchema))]
#[derive(Debug, Clone, Deserialize)]
pub struct ZoomParams {
    /// Path (absolute, or relative to the server's working directory) of the
    /// file to zoom into.
    pub path: String,
    /// Symbol name to zoom into (function, struct, class, heading, etc.).
    pub symbol: Option<String>,
    /// Start line (1-indexed) for line-range mode.
    pub start_line: Option<usize>,
    /// End line (1-indexed) for line-range mode.
    pub end_line: Option<usize>,
    /// Lines of context around the symbol body (default 3).
    pub context_lines: Option<usize>,
}

/// The `zoom` outcome: the header line plus the gutter-numbered snippet.
#[derive(Debug, Clone, Serialize)]
pub struct ZoomReport {
    /// The resolved path that was zoomed.
    pub path: String,
    /// Header line (`<kind> <name> (<lines>)` or `lines N-M`) followed by
    /// the numbered snippet.
    pub text: String,
}

/// Runs the `zoom` tool: by symbol, by line range, or (fallback) by textual
/// definition-prefix match.
pub fn run(_code: &CodeTools, params: ZoomParams) -> Result<ZoomReport> {
    let path = resolve_path(&params.path)?;
    let p = Path::new(&path);

    if !p.is_file() {
        return Err(tool_error(format!(
            "path is not a file: {}",
            relative_path(&path)
        )));
    }

    let content = std::fs::read_to_string(p).map_err(|e| tool_error(format!("read error: {e}")))?;
    _code.record_read(p);

    let context = params.context_lines.unwrap_or(DEFAULT_CONTEXT_LINES);

    if let Some(symbol_name) = &params.symbol {
        return zoom_by_symbol(&content, &path, symbol_name, context);
    }

    if let (Some(start), Some(end)) = (params.start_line, params.end_line) {
        return zoom_by_range(&content, &path, start, end, context);
    }

    Err(tool_error(
        "provide either `symbol` or both `start_line` and `end_line`",
    ))
}

fn zoom_by_symbol(
    content: &str,
    path: &str,
    symbol_name: &str,
    context: usize,
) -> Result<ZoomReport> {
    let p = Path::new(path);
    let lang = LangId::from_path(p);

    let Some(lang) = lang else {
        return text_search(content, path, symbol_name, context);
    };

    let symbols = outline::extract_symbols(content, lang);
    let matches: Vec<_> = symbols.iter().filter(|s| s.name == symbol_name).collect();

    if matches.is_empty() {
        return text_search(content, path, symbol_name, context);
    }

    if matches.len() > 1 {
        let candidates: Vec<String> = matches
            .iter()
            .map(|s| {
                format!(
                    "{}::{}:{} ({}-{})",
                    s.kind.label(),
                    s.name,
                    s.range.start_row + 1,
                    s.range.start_row + 1,
                    s.range.end_row + 1
                )
            })
            .collect();
        return Err(tool_error(format!(
            "ambiguous symbol \"{symbol_name}\"; candidates:\n{}",
            candidates.join("\n")
        )));
    }

    let sym = &matches[0];
    let start = sym.range.start_row.saturating_sub(context);
    let end = (sym.range.end_row + context).min(content.lines().count() - 1);

    let snippet = extract_lines(content, start, end);
    let header = format!(
        "{} {} ({}:{}-{})",
        sym.kind.label(),
        sym.name,
        sym.range.start_row + 1,
        sym.range.end_row + 1,
        relative_path(path)
    );

    Ok(ZoomReport {
        path: relative_path(path),
        text: format!("{header}\n{snippet}"),
    })
}

fn zoom_by_range(
    content: &str,
    path: &str,
    start_line: usize,
    end_line: usize,
    context: usize,
) -> Result<ZoomReport> {
    let total = content.lines().count();
    if start_line == 0 || start_line > total {
        return Err(tool_error(format!(
            "start_line {start_line} out of range (1-{total})"
        )));
    }
    if end_line < start_line {
        return Err(tool_error(format!(
            "end_line {end_line} must be >= start_line {start_line}"
        )));
    }

    let start = start_line.saturating_sub(1).saturating_sub(context);
    let end = (end_line - 1 + context).min(total - 1);

    let snippet = extract_lines(content, start, end);
    let header = format!("lines {}-{} {}", start + 1, end + 1, relative_path(path));

    Ok(ZoomReport {
        path: relative_path(path),
        text: format!("{header}\n{snippet}"),
    })
}

fn text_search(content: &str, path: &str, symbol_name: &str, context: usize) -> Result<ZoomReport> {
    let lines: Vec<&str> = content.lines().collect();
    let mut matches: Vec<usize> = Vec::new();

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        let is_heading = (trimmed.starts_with('#') || trimmed.starts_with("<h"))
            && trimmed.contains(symbol_name);
        let is_def = SYMBOL_PREFIXES
            .iter()
            .any(|p| trimmed.starts_with(&format!("{p}{symbol_name}")))
            || trimmed == symbol_name;

        if is_heading || is_def {
            matches.push(i);
        }
    }

    if matches.is_empty() {
        return Err(tool_error(format!(
            "symbol \"{symbol_name}\" not found in {}",
            relative_path(path)
        )));
    }

    if matches.len() > 1 {
        let candidates: Vec<String> = matches.iter().map(|&i| format!("line {}", i + 1)).collect();
        return Err(tool_error(format!(
            "ambiguous symbol \"{symbol_name}\"; found at:\n{}",
            candidates.join("\n")
        )));
    }

    let match_line = matches[0];
    let start = match_line.saturating_sub(context);
    let end = (match_line + context).min(lines.len() - 1);

    let snippet = extract_lines(content, start, end);
    let header = format!(
        "text match at line {} {}",
        match_line + 1,
        relative_path(path)
    );

    Ok(ZoomReport {
        path: relative_path(path),
        text: format!("{header}\n{snippet}"),
    })
}

const DEFAULT_CONTEXT_LINES: usize = 3;
const SYMBOL_PREFIXES: &[&str] = &[
    "",
    "fn ",
    "def ",
    "function ",
    "class ",
    "struct ",
    "impl ",
    "enum ",
    "pub fn ",
    "pub struct ",
];

fn extract_lines(content: &str, start: usize, end: usize) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let start = start.min(lines.len().saturating_sub(1));
    let end = end.min(lines.len().saturating_sub(1));

    let width = format!("{}", end + 1).len();
    let mut out = String::new();
    for (i, line) in lines[start..=end].iter().enumerate() {
        let ln = start + i + 1;
        let _ = std::fmt::write(&mut out, format_args!("{ln:>width$} | {line}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_lines_numbered() {
        let content = "a\nb\nc\nd\ne";
        let result = extract_lines(content, 1, 3);
        assert!(result.contains("2 | b"));
        assert!(result.contains("3 | c"));
        assert!(result.contains("4 | d"));
    }

    #[test]
    fn zoom_by_range_basic() {
        let content = "line1\nline2\nline3\nline4\nline5";
        let result = zoom_by_range(content, "/test.rs", 2, 4, 0).unwrap();
        assert!(result.text.contains("2 | line2"));
        assert!(result.text.contains("4 | line4"));
    }

    #[test]
    fn zoom_by_symbol_rust_fn() {
        let content = "fn greet() {\n    println!(\"hi\");\n}\nfn other() {}";
        let result = zoom_by_symbol(content, "/test.rs", "greet", 0).unwrap();
        assert!(result.text.contains("greet"));
        assert!(result.text.contains("1 |"));
    }

    /// Zooming a markdown heading returns the section's content, not just
    /// the heading line plus context.
    #[test]
    fn zoom_markdown_heading_returns_section_content() {
        let content =
            "# Guide\n\n## Error Handling\n\nfirst line of section\n\nmore detail\n\n## Next Section\n\nbye\n";
        let result = zoom_by_symbol(content, "/readme.md", "Error Handling", 0).unwrap();
        assert!(result.text.contains("Error Handling"));
        assert!(result.text.contains("first line of section"));
        assert!(result.text.contains("more detail"));
        assert!(!result.text.contains("bye"));
    }

    #[test]
    fn ambiguous_symbol_returns_candidates() {
        let content = "struct Foo {\n    x: i32,\n}\nimpl Foo {\n    fn foo() {}\n}\nfn foo() {}";
        let result = zoom_by_symbol(content, "/test.rs", "foo", 0);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("ambiguous"));
    }

    #[test]
    fn missing_symbol_returns_error() {
        let content = "nothing here";
        let result = text_search(content, "/test.txt", "nonexistent", 0);
        assert!(result.is_err());
    }

    #[test]
    fn zoom_range_out_of_bounds() {
        let content = "only\nthree\nlines";
        let result = zoom_by_range(content, "/test.rs", 100, 110, 0);
        assert!(result.is_err());
    }
}
