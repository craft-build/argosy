//! Symbol extraction: runs each language's tree-sitter query and folds the
//! captures into [`Symbol`]s, with bespoke walkers for YAML and TOML.

use tracing::error;
use tree_sitter::{Query, StreamingIterator};

use super::imports::parse_import_segments;
use super::lang::LangId;
use super::queries::lang_query;
use super::types::{Range, Symbol, SymbolKind};

pub fn extract_symbols(content: &str, lang: LangId) -> Vec<Symbol> {
    if matches!(lang, LangId::Yaml) {
        return extract_yaml_symbols(content, lang);
    }
    if matches!(lang, LangId::Toml) {
        return extract_toml_symbols(content, lang);
    }
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&lang.ts_language()).is_err() {
        error!(
            lang = lang.name(),
            "outline parser rejected language abi, skipping"
        );
        return vec![];
    }

    let Some(tree) = parser.parse(content, None) else {
        error!(
            lang = lang.name(),
            "outline parser returned no tree, skipping"
        );
        return vec![];
    };

    let query = match lang_query(lang) {
        Some(q) => q,
        None => return vec![],
    };

    let root = tree.root_node();
    let mut cursor = tree_sitter::QueryCursor::new();
    cursor.set_match_limit(65536);
    let mut matches = cursor.matches(query, root, content.as_bytes());

    let mut symbols = Vec::new();
    let mut seen_ranges = std::collections::HashSet::new();
    let import_sep = lang.import_separator();

    while let Some(m) = matches.next() {
        let mut name = String::new();
        let mut def_node: Option<tree_sitter::Node> = None;
        let mut kind = SymbolKind::Function;
        let mut is_child = false;

        for c in m.captures {
            let idx = c.index;
            let node = c.node;

            if is_name_capture(idx, query) {
                name = content[node.byte_range()].to_string();
            }

            if is_def_capture(idx, query) {
                def_node = Some(node);
                kind = def_capture_to_kind(idx, query);
                is_child = is_child_capture(idx, query);
            }
        }

        let Some(def_node) = def_node else { continue };
        if name.is_empty() {
            name = content[def_node.byte_range()]
                .lines()
                .next()
                .unwrap_or("")
                .to_string();
        }

        let start = def_node.start_position();
        let end = def_node.end_position();
        let key = (start.row, start.column, end.row, end.column);
        if !seen_ranges.insert(key) {
            continue;
        }

        let sig = content[def_node.byte_range()].to_string();
        let exported = is_exported(def_node, lang, content.as_bytes());
        let scope_chain = build_scope_chain(def_node, content.as_bytes());

        let import_segments = if kind == SymbolKind::Import {
            parse_import_segments(&sig, import_sep)
        } else {
            Vec::new()
        };

        symbols.push(Symbol {
            name,
            kind,
            range: Range {
                start_row: start.row,
                start_col: start.column,
                end_row: end.row,
                end_col: end.column,
            },
            signature: Some(sig),
            scope_chain,
            exported,
            import_segments,
            is_child,
        });
    }

    symbols.sort_by_key(|s| (s.range.start_row, s.range.start_col));

    if matches!(lang, LangId::Rust | LangId::Python) {
        for sym in &mut symbols {
            if sym.kind == SymbolKind::Function && sym.scope_chain.iter().any(|s| !s.is_empty()) {
                sym.kind = SymbolKind::Method;
            }
        }
    }

    symbols
}

fn extract_yaml_symbols(content: &str, lang: LangId) -> Vec<Symbol> {
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&lang.ts_language()).is_err() {
        error!(
            lang = lang.name(),
            "outline parser rejected language abi, skipping"
        );
        return vec![];
    }
    let Some(tree) = parser.parse(content, None) else {
        error!(
            lang = lang.name(),
            "outline parser returned no tree, skipping"
        );
        return vec![];
    };

    let source = content.as_bytes();
    let mut symbols = Vec::new();
    let root = tree.root_node();

    for stream_child in yaml_children(&root) {
        if stream_child.kind() != "document" {
            yaml_collect_pairs(&stream_child, source, &mut symbols, None);
            continue;
        }
        for doc_child in yaml_children(&stream_child) {
            yaml_collect_pairs(&doc_child, source, &mut symbols, None);
        }
    }

    symbols.sort_by_key(|s| (s.range.start_row, s.range.start_col));
    symbols
}

fn yaml_children<'a>(node: &tree_sitter::Node<'a>) -> Vec<tree_sitter::Node<'a>> {
    let mut cursor = node.walk();
    node.children(&mut cursor).collect()
}

fn yaml_collect_pairs(
    node: &tree_sitter::Node,
    source: &[u8],
    out: &mut Vec<Symbol>,
    parent_name: Option<&str>,
) {
    match node.kind() {
        "block_mapping" | "flow_mapping" => {}
        "block_node" | "flow_node" | "block_sequence" | "flow_sequence" | "block_sequence_item" => {
            for child in yaml_children(node) {
                yaml_collect_pairs(&child, source, out, parent_name);
            }
            return;
        }
        _ => return,
    }

    for pair in yaml_children(node)
        .iter()
        .filter(|c| matches!(c.kind(), "block_mapping_pair" | "flow_pair"))
    {
        let Some(key_node) = pair.child_by_field_name("key") else {
            continue;
        };
        let Some(name) = yaml_key_text(&key_node, source) else {
            continue;
        };

        let start = pair.start_position();
        let end = pair.end_position();
        let scope_chain = parent_name.map(|p| vec![p.to_string()]).unwrap_or_default();
        out.push(Symbol {
            name: name.clone(),
            kind: SymbolKind::Constant,
            range: Range {
                start_row: start.row,
                start_col: start.column,
                end_row: end.row,
                end_col: end.column,
            },
            signature: None,
            scope_chain,
            exported: false,
            import_segments: Vec::new(),
            is_child: false,
        });

        if parent_name.is_none()
            && let Some(value) = pair.child_by_field_name("value")
        {
            yaml_collect_pairs(&value, source, out, Some(&name));
        }
    }
}

fn yaml_key_text(node: &tree_sitter::Node, source: &[u8]) -> Option<String> {
    let raw = node.utf8_text(source).ok()?;
    let trimmed = raw.trim();
    let unquoted = trimmed
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| {
            trimmed
                .strip_prefix('\'')
                .and_then(|s| s.strip_suffix('\''))
        })
        .unwrap_or(trimmed);
    if unquoted.is_empty() {
        None
    } else {
        Some(unquoted.to_string())
    }
}

pub(super) const TOML_FIELD_TRUNCATE_THRESHOLD: usize = 8;
const TOML_VALUE_TRUNCATE: usize = 60;

fn extract_toml_symbols(content: &str, lang: LangId) -> Vec<Symbol> {
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&lang.ts_language()).is_err() {
        error!(
            lang = lang.name(),
            "outline parser rejected language abi, skipping"
        );
        return vec![];
    }
    let Some(tree) = parser.parse(content, None) else {
        error!(
            lang = lang.name(),
            "outline parser returned no tree, skipping"
        );
        return vec![];
    };

    let source = content.as_bytes();
    let mut symbols = Vec::new();
    let root = tree.root_node();

    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        match child.kind() {
            "pair" => {
                if let Some(sym) = toml_pair_symbol(&child, source, None, true) {
                    symbols.push(sym);
                }
            }
            "table" => toml_push_table(&child, source, false, &mut symbols),
            "table_array_element" => toml_push_table(&child, source, true, &mut symbols),
            _ => {}
        }
    }

    symbols.sort_by_key(|s| (s.range.start_row, s.range.start_col));
    symbols
}

fn toml_key_node<'a>(node: &tree_sitter::Node<'a>) -> Option<tree_sitter::Node<'a>> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "bare_key" | "dotted_key" | "quoted_key" => return Some(child),
            _ => {}
        }
    }
    None
}

fn toml_value_node<'a>(node: &tree_sitter::Node<'a>) -> Option<tree_sitter::Node<'a>> {
    let mut cursor = node.walk();
    let mut seen_key = false;
    for child in node.children(&mut cursor) {
        match child.kind() {
            "bare_key" | "dotted_key" | "quoted_key" => seen_key = true,
            "string" | "integer" | "float" | "boolean" | "offset_date_time" | "local_date_time"
            | "local_date" | "local_time" | "array" | "inline_table"
                if seen_key =>
            {
                return Some(child);
            }
            _ => {}
        }
    }
    None
}

fn toml_pair_symbol(
    node: &tree_sitter::Node,
    source: &[u8],
    parent: Option<&str>,
    include_value: bool,
) -> Option<Symbol> {
    let key_node = toml_key_node(node)?;
    let name = key_node.utf8_text(source).ok()?.trim().to_string();
    if name.is_empty() {
        return None;
    }
    let signature = if include_value {
        toml_value_node(node)
            .and_then(|v| v.utf8_text(source).ok())
            .map(|raw| {
                let collapsed: String = raw.split_ascii_whitespace().collect::<Vec<_>>().join(" ");
                let trimmed = collapsed.trim();
                let sig = format!("{name} = {trimmed}");
                if sig.chars().count() > TOML_VALUE_TRUNCATE {
                    let boundary = sig.floor_char_boundary(TOML_VALUE_TRUNCATE);
                    format!("{}…", &sig[..boundary])
                } else {
                    sig
                }
            })
    } else {
        None
    };
    let start = node.start_position();
    let end = node.end_position();
    let scope_chain = parent.map(|p| vec![p.to_string()]).unwrap_or_default();
    Some(Symbol {
        name,
        kind: SymbolKind::Constant,
        range: Range {
            start_row: start.row,
            start_col: start.column,
            end_row: end.row,
            end_col: end.column,
        },
        signature,
        scope_chain,
        exported: false,
        import_segments: Vec::new(),
        is_child: false,
    })
}

fn toml_push_table(node: &tree_sitter::Node, source: &[u8], is_array: bool, out: &mut Vec<Symbol>) {
    let header_path = toml_key_node(node)
        .and_then(|k| k.utf8_text(source).ok())
        .map(|t| t.trim().to_string())
        .unwrap_or_else(|| "?".to_string());
    let label = if is_array {
        format!("[[{header_path}]]")
    } else {
        format!("[{header_path}]")
    };
    let start = node.start_position();
    let end = node.end_position();
    out.push(Symbol {
        name: label.clone(),
        kind: SymbolKind::Constant,
        range: Range {
            start_row: start.row,
            start_col: start.column,
            end_row: end.row,
            end_col: end.column,
        },
        signature: None,
        scope_chain: Vec::new(),
        exported: false,
        import_segments: Vec::new(),
        is_child: false,
    });

    let mut cursor = node.walk();
    let pair_nodes: Vec<tree_sitter::Node> = node
        .children(&mut cursor)
        .filter(|c| c.kind() == "pair")
        .collect();
    for (i, pair) in pair_nodes.iter().enumerate() {
        let include_value = i < TOML_FIELD_TRUNCATE_THRESHOLD;
        if let Some(sym) = toml_pair_symbol(pair, source, Some(&label), include_value) {
            out.push(sym);
        }
    }
}

fn is_name_capture(idx: u32, query: &Query) -> bool {
    const NAMES: &[&str] = &[
        "fn.name",
        "method.name",
        "struct.name",
        "enum.name",
        "trait.name",
        "type.name",
        "const.name",
        "mod.name",
        "impl.name",
        "macro.name",
        "class.name",
        "iface.name",
        "var.name",
        "heading.name",
        "import.name",
        "field.name",
        "variant.name",
    ];
    NAMES
        .iter()
        .any(|n| query.capture_index_for_name(n).unwrap_or(u32::MAX) == idx)
}

fn is_def_capture(idx: u32, query: &Query) -> bool {
    const DEFS: &[&str] = &[
        "fn.def",
        "method.def",
        "struct.def",
        "enum.def",
        "trait.def",
        "type.def",
        "const.def",
        "mod.def",
        "impl.def",
        "macro.def",
        "class.def",
        "iface.def",
        "var.def",
        "heading.def",
        "import.def",
        "field.def",
        "variant.def",
    ];
    DEFS.iter()
        .any(|n| query.capture_index_for_name(n).unwrap_or(u32::MAX) == idx)
}

fn def_capture_to_kind(idx: u32, query: &Query) -> SymbolKind {
    const PAIRS: &[(&str, SymbolKind)] = &[
        ("fn.def", SymbolKind::Function),
        ("method.def", SymbolKind::Method),
        ("struct.def", SymbolKind::Struct),
        ("enum.def", SymbolKind::Enum),
        ("trait.def", SymbolKind::Trait),
        ("type.def", SymbolKind::TypeAlias),
        ("const.def", SymbolKind::Constant),
        ("mod.def", SymbolKind::Module),
        ("impl.def", SymbolKind::Impl),
        ("macro.def", SymbolKind::Macro),
        ("class.def", SymbolKind::Class),
        ("iface.def", SymbolKind::Interface),
        ("var.def", SymbolKind::Variable),
        ("heading.def", SymbolKind::Heading),
        ("import.def", SymbolKind::Import),
        ("field.def", SymbolKind::Variable),
        ("variant.def", SymbolKind::Variable),
    ];
    PAIRS
        .iter()
        .find(|(n, _)| query.capture_index_for_name(n).unwrap_or(u32::MAX) == idx)
        .map(|(_, k)| *k)
        .unwrap_or(SymbolKind::Variable)
}

fn is_child_capture(idx: u32, query: &Query) -> bool {
    const CHILD_DEFS: &[&str] = &["field.def", "variant.def"];
    CHILD_DEFS
        .iter()
        .any(|n| query.capture_index_for_name(n).unwrap_or(u32::MAX) == idx)
}

fn is_exported(node: tree_sitter::Node, lang: LangId, source: &[u8]) -> bool {
    match lang {
        LangId::Rust => {
            if node.child_by_field_name("visibility").is_some() {
                return true;
            }
            if let Some(sibling) = node.prev_named_sibling()
                && sibling.kind() == "visibility_modifier"
            {
                return true;
            }
            false
        }
        LangId::Go => {
            if let Some(name_node) = node.child_by_field_name("name") {
                name_node
                    .utf8_text(source)
                    .is_ok_and(|s| s.chars().next().is_some_and(|c| c.is_uppercase()))
            } else {
                false
            }
        }
        _ => false,
    }
}

fn build_scope_chain(node: tree_sitter::Node, source: &[u8]) -> Vec<String> {
    let mut chain = Vec::new();
    let mut current = node.parent();

    while let Some(parent) = current {
        match parent.kind() {
            "source_file" | "declaration_list" => {}
            "impl_item" => {
                if let Some(type_node) = parent.child_by_field_name("type")
                    && let Ok(txt) = type_node.utf8_text(source)
                {
                    chain.push(txt.to_string());
                }
            }
            _ => {
                if let Some(name_node) = parent.child_by_field_name("name")
                    && let Ok(txt) = name_node.utf8_text(source)
                    && !txt.is_empty()
                {
                    chain.push(txt.to_string());
                }
            }
        }
        current = parent.parent();
    }

    chain.reverse();
    chain
}
