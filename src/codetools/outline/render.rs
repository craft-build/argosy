//! Tree building and rendering: folds extracted symbols into a nested
//! outline and renders file trees, directory listings, and file tables.

use std::borrow::Cow;

use super::imports::{TrieNode, render_trie};
use super::lang::LangId;
use super::types::{DirEntry, OutlineEntry, Range, Symbol, SymbolKind};

const MAX_OUTPUT_BYTES: usize = 30_000;

pub(super) fn build_outline_tree(symbols: &[Symbol]) -> Vec<OutlineEntry> {
    let mut root: Vec<OutlineEntry> = Vec::new();

    for sym in symbols {
        let entry = OutlineEntry {
            name: sym.name.clone(),
            kind: sym.kind,
            range: sym.range.clone(),
            signature: sym.signature.clone(),
            exported: sym.exported,
            members: vec![],
            import_segments: sym.import_segments.clone(),
        };

        if sym.is_child {
            attach_as_member(&mut root, sym);
        } else {
            insert_at_scope(&mut root, entry, &sym.scope_chain);
        }
    }

    root
}

fn attach_as_member(root: &mut [OutlineEntry], sym: &Symbol) {
    for entry in root.iter_mut().rev() {
        if range_contains(&entry.range, &sym.range)
            && matches!(
                entry.kind,
                SymbolKind::Struct | SymbolKind::Enum | SymbolKind::Class | SymbolKind::Impl
            )
        {
            entry.members.push(OutlineEntry {
                name: sym.name.clone(),
                kind: sym.kind,
                range: sym.range.clone(),
                signature: None,
                exported: false,
                members: vec![],
                import_segments: vec![],
            });
            return;
        }
        if !entry.members.is_empty() {
            attach_as_member(&mut entry.members, sym);
            return;
        }
    }
}

fn range_contains(outer: &Range, inner: &Range) -> bool {
    (inner.start_row, inner.start_col) >= (outer.start_row, outer.start_col)
        && (inner.end_row, inner.end_col) <= (outer.end_row, outer.end_col)
}

fn insert_at_scope(entries: &mut Vec<OutlineEntry>, entry: OutlineEntry, scope: &[String]) {
    if scope.is_empty() {
        entries.push(entry);
        return;
    }

    let head = &scope[0];
    if let Some(parent) = entries.iter_mut().find(|e| e.name == *head) {
        insert_at_scope(&mut parent.members, entry, &scope[1..]);
    } else {
        entries.push(entry);
    }
}

pub(super) fn count_leaves(entries: &[OutlineEntry]) -> usize {
    entries
        .iter()
        .map(|e| {
            if e.members.is_empty() {
                1
            } else {
                count_leaves(&e.members)
            }
        })
        .sum()
}

pub(super) fn render_file_outline(
    filename: &str,
    entries: &[OutlineEntry],
    lang: LangId,
) -> (String, bool) {
    let mut out = String::new();
    out.push_str(filename);
    out.push('\n');

    let imports: Vec<&OutlineEntry> = entries
        .iter()
        .filter(|e| e.kind == SymbolKind::Import)
        .collect();
    let non_imports: Vec<&OutlineEntry> = entries
        .iter()
        .filter(|e| e.kind != SymbolKind::Import)
        .collect();
    if !imports.is_empty() {
        let mut min_line = usize::MAX;
        let mut max_line = 0usize;
        for e in &imports {
            min_line = min_line.min(e.range.start_row);
            max_line = max_line.max(e.range.end_row);
        }
        let _ = std::fmt::write(
            &mut out,
            format_args!("  imports: [{}-{}]\n", min_line + 1, max_line + 1),
        );
        let mut trie = TrieNode::new();
        for e in &imports {
            for path in &e.import_segments {
                trie.insert(path);
            }
        }
        let sep = lang.import_separator();
        for line in render_trie(&trie, sep) {
            let _ = std::fmt::write(&mut out, format_args!("    {line}\n"));
        }
        out.push('\n');
    }

    render_entries(&non_imports, 1, &mut out);
    truncate_outline(&mut out)
}

fn render_entries(entries: &[&OutlineEntry], depth: usize, out: &mut String) {
    let indent = "  ".repeat(depth);
    for entry in entries {
        if entry.kind == SymbolKind::Import {
            continue;
        }
        let exp = if entry.exported { "E" } else { " " };
        let kind = entry.kind.label();
        let sig = entry
            .signature
            .as_deref()
            .map(truncate_signature)
            .unwrap_or_else(|| entry.name.clone());

        let _ = std::fmt::write(
            out,
            format_args!(
                "{indent}{exp} {kind:2} {sig} {}:{}\n",
                entry.range.start_row + 1,
                entry.range.start_col + 1,
            ),
        );

        if !entry.members.is_empty() {
            render_members(&entry.members, depth + 1, out);
        }
    }
}

const MAX_INLINE_MEMBERS: usize = 8;

fn render_entries_owned(entries: &[OutlineEntry], depth: usize, out: &mut String) {
    let refs: Vec<&OutlineEntry> = entries.iter().collect();
    render_entries(&refs, depth, out);
}

fn render_members(members: &[OutlineEntry], depth: usize, out: &mut String) {
    let indent = "  ".repeat(depth);
    let total = members.len();
    let show = total.min(MAX_INLINE_MEMBERS);
    let mut names: Vec<Cow<'_, str>> = members[..show]
        .iter()
        .map(|m| Cow::Borrowed(m.name.as_str()))
        .collect();
    if total > MAX_INLINE_MEMBERS {
        names.push(Cow::Owned(format!("[{} more]", total - MAX_INLINE_MEMBERS)));
    }
    let mut line = indent.clone();
    for (i, name) in names.iter().enumerate() {
        if i > 0 {
            line.push_str(", ");
        }
        line.push_str(name);
        if line.len() > 100 {
            let _ = std::fmt::write(out, format_args!("{line}\n"));
            line = indent.clone();
            line.push_str(name);
        }
    }
    if !line.trim().is_empty() {
        let _ = std::fmt::write(out, format_args!("{line}\n"));
    }
}

pub(super) fn truncate_signature(sig: &str) -> String {
    let first_line = sig.lines().next().unwrap_or(sig);
    if first_line.len() > 80 {
        let boundary = first_line.floor_char_boundary(79);
        format!("{}…", &first_line[..boundary])
    } else {
        first_line.to_string()
    }
}

pub(super) fn render_dir_outline(
    entries: &[DirEntry],
    skipped: &[String],
    total_bytes: usize,
) -> (String, bool) {
    let mut out = String::new();
    for e in entries {
        out.push_str(&e.rel_path);
        out.push('\n');
        render_entries_owned(&e.tree, 1, &mut out);
        out.push('\n');
    }
    if !skipped.is_empty() {
        out.push_str("skipped:\n");
        for s in skipped {
            let _ = std::fmt::write(&mut out, format_args!("  {s}\n"));
        }
    }
    let _ = std::fmt::write(
        &mut out,
        format_args!("total: {} files, {} bytes\n", entries.len(), total_bytes),
    );
    truncate_outline(&mut out)
}

pub(super) fn render_files_table(entries: &[DirEntry], skipped: &[String]) -> (String, bool) {
    let mut out = String::new();
    out.push_str("path                                   lang        symbols  bytes\n");
    out.push_str("─────────────────────────────────────────────────────────────────────\n");
    for e in entries {
        let _ = std::fmt::write(
            &mut out,
            format_args!(
                "{:<40} {:<12} {:>7}  {:>6}\n",
                e.rel_path,
                e.lang.name(),
                e.symbol_count,
                e.bytes,
            ),
        );
    }
    if !skipped.is_empty() {
        out.push('\n');
        out.push_str("skipped:\n");
        for s in skipped {
            let _ = std::fmt::write(&mut out, format_args!("  {s}\n"));
        }
    }
    truncate_outline(&mut out)
}

pub(super) fn truncate_outline(out: &mut String) -> (String, bool) {
    let truncated = out.len() > MAX_OUTPUT_BYTES;
    if truncated {
        let truncation_hint = "\n… (output truncated, narrow the path to see more)";
        // Outline text routinely contains multibyte characters; flooring
        // to a char boundary keeps `truncate` from panicking when one
        // straddles the cut.
        let cut = out.floor_char_boundary(MAX_OUTPUT_BYTES - truncation_hint.len());
        out.truncate(cut);
        out.push_str(truncation_hint);
    }
    (std::mem::take(out), truncated)
}
