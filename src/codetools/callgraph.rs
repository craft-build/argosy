//! The `callgraph` tool: intra-file call-graph analysis, ported from Craft's
//! `tools/callgraph.rs`. Reuses [`outline`]'s language registry and symbol
//! extraction, then adds a call-extraction pass of its own.

use std::collections::HashSet;

#[cfg(feature = "mcp")]
use rmcp::schemars;

use serde::{Deserialize, Serialize};

use super::outline::{LangId, Range, Symbol, extract_symbols};
use super::{CodeTools, relative_path, resolve_path, tool_error};
use crate::error::Result;

/// `callgraph` parameters.
#[cfg_attr(feature = "mcp", derive(rmcp::schemars::JsonSchema))]
#[derive(Debug, Clone, Deserialize)]
pub struct CallgraphParams {
    /// Operation: `call_tree`, `callers`, or `impact`.
    pub op: String,
    /// Path (absolute, or relative to the server's working directory) of the
    /// file to analyze.
    pub path: String,
    /// Symbol name (function/method/struct).
    pub symbol: String,
    /// Max depth for `call_tree` (default 5).
    pub depth: Option<usize>,
}

/// The `callgraph` outcome: the rendered tree or list.
#[derive(Debug, Clone, Serialize)]
pub struct CallgraphReport {
    /// The operation that ran.
    pub op: String,
    /// The target symbol.
    pub symbol: String,
    /// The rendered call tree (`call_tree`) or symbol list (`callers`,
    /// `impact`).
    pub text: String,
}

/// Runs the `callgraph` tool over a single file.
pub fn run(_code: &CodeTools, params: CallgraphParams) -> Result<CallgraphReport> {
    // Language detection runs on the raw input path (as in Craft), so plain
    // relative paths like `src/main.rs` work without resolution first.
    let lang = LangId::from_path(params.path.as_ref()).ok_or_else(|| {
        tool_error(format!(
            "unsupported file type: {}",
            relative_path(&params.path)
        ))
    })?;

    let resolved = resolve_path(&params.path)?;
    let p = std::path::Path::new(&resolved);
    let content = std::fs::read_to_string(p).map_err(|e| tool_error(format!("read error: {e}")))?;
    _code.record_read(p);

    let symbols = extract_symbols(&content, lang);
    let calls = extract_calls(&content, lang);

    let target = find_symbol(&symbols, &params.symbol)?;

    match params.op.as_str() {
        "call_tree" => {
            let max_depth = params.depth.unwrap_or(5);
            let tree = build_call_tree(target, &symbols, &calls, max_depth);
            Ok(CallgraphReport {
                op: params.op,
                symbol: params.symbol,
                text: render_call_tree(&tree, 0),
            })
        }
        "callers" => {
            let callers = find_callers(target, &symbols, &calls);
            Ok(CallgraphReport {
                op: params.op,
                symbol: params.symbol,
                text: render_symbol_list("callers", &target.name, &callers),
            })
        }
        "impact" => {
            let impacted = find_impact(target, &symbols, &calls);
            Ok(CallgraphReport {
                op: params.op,
                symbol: params.symbol,
                text: render_symbol_list("impact", &target.name, &impacted),
            })
        }
        _ => Err(tool_error(format!(
            "unknown op \"{}\"; use call_tree, callers, or impact",
            params.op
        ))),
    }
}

fn find_symbol<'a>(symbols: &'a [Symbol], name: &str) -> Result<&'a Symbol> {
    let matches: Vec<&Symbol> = symbols.iter().filter(|s| s.name == name).collect();
    match matches.len() {
        0 => {
            let candidates: Vec<String> = symbols
                .iter()
                .filter(|s| s.name.contains(name))
                .map(|s| s.name.clone())
                .take(10)
                .collect();
            let hint = if candidates.is_empty() {
                String::new()
            } else {
                format!(" similar: {}", candidates.join(", "))
            };
            Err(tool_error(format!(
                "symbol \"{name}\" not found in file.{hint}"
            )))
        }
        1 => Ok(matches[0]),
        n => {
            let disambig: Vec<String> = matches
                .iter()
                .map(|s| format!("{} (line {})", s.name, s.range.start_row + 1))
                .collect();
            Err(tool_error(format!(
                "symbol \"{name}\" is ambiguous ({n} matches): {}",
                disambig.join(", ")
            )))
        }
    }
}

#[derive(Debug, Clone)]
struct RawCall {
    name: String,
    line: usize,
}

fn extract_calls(content: &str, lang: LangId) -> Vec<RawCall> {
    let source = content.as_bytes();
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&lang.ts_language()).is_err() {
        tracing::error!(
            lang = lang.name(),
            "callgraph parser rejected language abi, skipping"
        );
        return Vec::new();
    }

    let Some(tree) = parser.parse(source, None) else {
        tracing::error!(
            lang = lang.name(),
            "callgraph parser returned no tree, skipping"
        );
        return Vec::new();
    };
    let root = tree.root_node();

    let mut calls = Vec::new();
    walk_for_calls(root, source, lang, &mut calls);
    calls
}

fn walk_for_calls(root: tree_sitter::Node, source: &[u8], lang: LangId, calls: &mut Vec<RawCall>) {
    let mut cursor = root.walk();
    loop {
        let node = cursor.node();
        if is_call_node(node.kind(), lang)
            && let Some(name) = extract_call_name(node, source, lang)
        {
            calls.push(RawCall {
                name,
                line: node.start_position().row,
            });
        }
        if !cursor.goto_first_child() {
            while !cursor.goto_next_sibling() {
                if !cursor.goto_parent() {
                    return;
                }
            }
        }
    }
}

fn is_call_node(kind: &str, lang: LangId) -> bool {
    match lang {
        LangId::Rust => kind == "call_expression",
        LangId::TypeScript => kind == "call_expression",
        LangId::Python => kind == "call",
        LangId::Go => kind == "call_expression",
        LangId::Java => kind == "method_invocation" || kind == "class_instance_creation_expression",
        LangId::C | LangId::Cpp => kind == "call_expression",
        LangId::Ruby => kind == "call",
        LangId::Lua => kind == "function_call",
        _ => kind == "call_expression",
    }
}

fn extract_call_name(node: tree_sitter::Node, source: &[u8], _lang: LangId) -> Option<String> {
    let func_node = node.child_by_field_name("function")?;

    let text = func_node.utf8_text(source).ok()?;
    let name = if text.contains('.') {
        text.rsplit('.').next().unwrap_or(text).to_string()
    } else {
        text.to_string()
    };

    if name.is_empty() || name.starts_with('$') || name.starts_with('<') {
        None
    } else {
        Some(name)
    }
}

fn calls_in_range(calls: &[RawCall], range: &Range) -> Vec<usize> {
    calls
        .iter()
        .enumerate()
        .filter(|(_, c)| c.line >= range.start_row && c.line <= range.end_row)
        .map(|(i, _)| i)
        .collect()
}

#[derive(Debug)]
struct CallTreeNode {
    name: String,
    line: usize,
    /// True when this edge closes a cycle back to a symbol already on the
    /// path — rendered as a marked leaf rather than silently pruned.
    recursive: bool,
    children: Vec<CallTreeNode>,
}

fn build_call_tree(
    symbol: &Symbol,
    symbols: &[Symbol],
    calls: &[RawCall],
    max_depth: usize,
) -> CallTreeNode {
    build_call_tree_inner(symbol, symbols, calls, max_depth, &mut HashSet::new())
}

fn build_call_tree_inner(
    symbol: &Symbol,
    symbols: &[Symbol],
    calls: &[RawCall],
    remaining: usize,
    visited: &mut HashSet<String>,
) -> CallTreeNode {
    let in_scope_calls = calls_in_range(calls, &symbol.range);

    // A symbol already on the current path marks a cycle: keep the edge
    // (dropping it would hide the recursion) but do not descend.
    let fresh = visited.insert(symbol.name.clone());
    let recursive = remaining > 0 && !fresh;
    let mut children = Vec::new();
    if remaining > 0 && fresh {
        for &idx in &in_scope_calls {
            let call = &calls[idx];
            if let Some(called) = symbols.iter().find(|s| s.name == call.name) {
                children.push(build_call_tree_inner(
                    called,
                    symbols,
                    calls,
                    remaining - 1,
                    visited,
                ));
            } else {
                children.push(CallTreeNode {
                    name: call.name.clone(),
                    line: call.line,
                    recursive: false,
                    children: Vec::new(),
                });
            }
        }
        visited.remove(&symbol.name);
    }

    CallTreeNode {
        name: symbol.name.clone(),
        line: symbol.range.start_row,
        recursive,
        children,
    }
}

fn find_callers<'a>(target: &Symbol, symbols: &'a [Symbol], calls: &[RawCall]) -> Vec<&'a Symbol> {
    symbols
        .iter()
        .filter(|s| {
            s.range.start_row != target.range.start_row
                && calls_in_range(calls, &s.range)
                    .iter()
                    .any(|&i| calls[i].name == target.name)
        })
        .collect()
}

fn find_impact<'a>(target: &Symbol, symbols: &'a [Symbol], calls: &[RawCall]) -> Vec<&'a Symbol> {
    let mut impacted = Vec::new();
    let mut queue = vec![target.name.clone()];
    let mut seen: HashSet<String> = [target.name.clone()].into_iter().collect();

    while let Some(name) = queue.pop() {
        let callers: Vec<&Symbol> = symbols
            .iter()
            .filter(|s| {
                !seen.contains(&s.name)
                    && calls_in_range(calls, &s.range)
                        .iter()
                        .any(|&i| calls[i].name == name)
            })
            .collect();

        for caller in &callers {
            seen.insert(caller.name.clone());
            queue.push(caller.name.clone());
            impacted.push(*caller);
        }
    }

    impacted
}

/// Like every other code tool, the rendered tree is capped; with a deep
/// `depth` and diamond call patterns the tree is exponential, so rendering
/// also stops early once past the cap instead of building it all first.
const MAX_OUTPUT_BYTES: usize = 30_000;

fn render_call_tree(node: &CallTreeNode, depth: usize) -> String {
    let mut out = String::new();
    let stopped = render_call_tree_inner(node, depth, &mut out);
    if stopped || out.len() > MAX_OUTPUT_BYTES {
        let hint = "\n… (output truncated, narrow the depth to see more)";
        let cut = out.floor_char_boundary(MAX_OUTPUT_BYTES.saturating_sub(hint.len()));
        out.truncate(cut);
        out.push_str(hint);
    }
    out
}

/// Returns true once past the output cap, so callers stop rendering.
fn render_call_tree_inner(node: &CallTreeNode, depth: usize, out: &mut String) -> bool {
    if out.len() > MAX_OUTPUT_BYTES {
        return true;
    }
    let indent = "  ".repeat(depth);
    let marker = if node.recursive { " [recursive]" } else { "" };
    let _ = std::fmt::write(
        out,
        format_args!(
            "{}{} (line {}){}\n",
            indent,
            node.name,
            node.line + 1,
            marker
        ),
    );
    for child in &node.children {
        if render_call_tree_inner(child, depth + 1, out) {
            return true;
        }
    }
    false
}

fn render_symbol_list(label: &str, target_name: &str, symbols: &[&Symbol]) -> String {
    let mut out = format!("{label} of \"{target_name}\"\n");
    if symbols.is_empty() {
        out.push_str("  (none found in this file)\n");
    } else {
        for s in symbols {
            let _ = std::fmt::write(
                &mut out,
                format_args!(
                    "  {} {} (line {})\n",
                    s.kind.label(),
                    s.name,
                    s.range.start_row + 1
                ),
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rust_symbols() -> Vec<Symbol> {
        let code = RUST_CODE;
        extract_symbols(code, LangId::Rust)
    }

    fn rust_calls() -> Vec<RawCall> {
        extract_calls(RUST_CODE, LangId::Rust)
    }

    const RUST_CODE: &str = r#"
fn main() {
    foo();
    bar();
}

fn foo() {
    baz();
    external();
}

fn bar() {
    baz();
}

fn baz() {
    println!("hi");
}
"#;

    #[test]
    fn find_symbol_returns_matching() {
        let syms = rust_symbols();
        let result = find_symbol(&syms, "main");
        assert!(result.is_ok());
        assert_eq!(result.unwrap().name, "main");
    }

    #[test]
    fn find_symbol_rejects_unknown() {
        let syms = rust_symbols();
        let result = find_symbol(&syms, "nonexistent");
        assert!(result.is_err());
    }

    #[test]
    fn find_callers_finds_direct() {
        let syms = rust_symbols();
        let calls = rust_calls();
        let target = find_symbol(&syms, "baz").unwrap();
        let callers = find_callers(target, &syms, &calls);
        let names: Vec<&str> = callers.iter().map(|s| s.name.as_str()).collect();
        assert!(
            names.contains(&"foo"),
            "expected foo in callers, got: {names:?}"
        );
        assert!(
            names.contains(&"bar"),
            "expected bar in callers, got: {names:?}"
        );
    }

    #[test]
    fn find_impact_traverses_transitively() {
        let syms = rust_symbols();
        let calls = rust_calls();
        let target = find_symbol(&syms, "baz").unwrap();
        let impacted = find_impact(target, &syms, &calls);
        let names: Vec<&str> = impacted.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"foo"), "expected foo in impact");
        assert!(names.contains(&"bar"), "expected bar in impact");
        assert!(names.contains(&"main"), "expected main in impact");
    }

    #[test]
    fn call_tree_builds_hierarchy() {
        let syms = rust_symbols();
        let calls = rust_calls();
        let target = find_symbol(&syms, "main").unwrap();
        let tree = build_call_tree(target, &syms, &calls, 5);
        assert_eq!(tree.name, "main");
        let child_names: Vec<&str> = tree.children.iter().map(|c| c.name.as_str()).collect();
        assert!(
            child_names.contains(&"foo"),
            "expected foo in call tree children"
        );
        assert!(
            child_names.contains(&"bar"),
            "expected bar in call tree children"
        );
    }

    /// A self-recursive call must render as a marked leaf, not silently
    /// disappear or masquerade as a plain leaf.
    #[test]
    fn recursive_calls_render_as_marked_leaves() {
        let src = "fn walk(n: u32) {\n    if n > 0 {\n        walk(n - 1);\n    }\n}\n";
        let syms = extract_symbols(src, LangId::Rust);
        let calls = extract_calls(src, LangId::Rust);
        let target = find_symbol(&syms, "walk").unwrap();
        let tree = build_call_tree(target, &syms, &calls, 5);
        let rendered = render_call_tree(&tree, 0);
        assert!(
            rendered.contains("walk (line 1) [recursive]"),
            "got {rendered}"
        );
    }

    /// The rendered tree is capped like every other code tool's output.
    #[test]
    fn call_tree_render_caps_output() {
        let children: Vec<CallTreeNode> = (0..30_000)
            .map(|i| CallTreeNode {
                name: format!("function_with_a_long_name_{i}"),
                line: i,
                recursive: false,
                children: Vec::new(),
            })
            .collect();
        let node = CallTreeNode {
            name: "root".into(),
            line: 0,
            recursive: false,
            children,
        };
        let text = render_call_tree(&node, 0);
        assert!(text.len() < MAX_OUTPUT_BYTES + 200, "got {}", text.len());
        assert!(text.contains("output truncated"), "got:\n{text}");
    }

    #[test]
    fn run_reports_ops() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("graph.rs");
        std::fs::write(&file, RUST_CODE).unwrap();

        let tools = super::CodeTools::default();
        let report = run(
            &tools,
            CallgraphParams {
                op: "call_tree".into(),
                path: file.to_string_lossy().into_owned(),
                symbol: "main".into(),
                depth: None,
            },
        )
        .unwrap();
        assert!(report.text.contains("main (line 2)"), "got {}", report.text);
        assert!(report.text.contains("foo (line 7)"));

        let report = run(
            &tools,
            CallgraphParams {
                op: "callers".into(),
                path: file.to_string_lossy().into_owned(),
                symbol: "baz".into(),
                depth: None,
            },
        )
        .unwrap();
        assert!(report.text.contains("callers of \"baz\""));

        let err = run(
            &tools,
            CallgraphParams {
                op: "nope".into(),
                path: file.to_string_lossy().into_owned(),
                symbol: "main".into(),
                depth: None,
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("unknown op"), "{err}");
    }
}
