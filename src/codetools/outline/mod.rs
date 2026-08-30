//! The `outline` tool: a structural outline of a file or directory,
//! ported from Craft's `tools/outline.rs`. Also the de-facto language
//! registry for the code tools — `zoom` and `callgraph` reuse [`LangId`]
//! and [`extract_symbols`].

mod extract;
mod imports;
mod lang;
mod queries;
mod render;
mod types;

#[cfg(test)]
mod tests;

pub use extract::extract_symbols;
pub use lang::LangId;
pub use types::{Range, Symbol, SymbolKind};

use std::path::Path;

// The `JsonSchema` derive expansion references the `schemars` crate by
// bare name; aliasing rmcp's re-export keeps our version pinned to the
// SDK's.
#[cfg(feature = "mcp")]
use rmcp::schemars;

use serde::{Deserialize, Serialize};

use super::{CodeTools, relative_path, resolve_path, tool_error};
use crate::error::Result;

use render::{
    build_outline_tree, count_leaves, render_dir_outline, render_file_outline, render_files_table,
};
use types::DirEntry;

/// `outline` parameters.
#[cfg_attr(feature = "mcp", derive(rmcp::schemars::JsonSchema))]
#[derive(Debug, Clone, Deserialize)]
pub struct OutlineParams {
    /// Path (absolute, or relative to the server's working directory) of a
    /// file or directory.
    pub path: String,
    /// When `path` is a directory, return a flat file table instead of
    /// nested symbols.
    pub files: Option<bool>,
}

/// The `outline` outcome: the rendered outline plus its envelope.
#[derive(Debug, Clone, Serialize)]
pub struct OutlineReport {
    /// The resolved path that was outlined.
    pub path: String,
    /// The rendered outline (file tree, directory listing, or file table),
    /// capped at 30 KB.
    pub text: String,
    /// True iff the rendering hit the 30 KB cap — narrow the path to see
    /// more.
    pub truncated: bool,
}

/// Runs the `outline` tool: one file renders its symbol tree; a directory
/// renders per-file trees (or a flat file table with `files`).
pub fn run(_code: &CodeTools, params: OutlineParams) -> Result<OutlineReport> {
    let path = resolve_path(&params.path)?;
    let p = Path::new(&path);

    if p.is_dir() {
        let (text, truncated) = outline_dir(&path, params.files.unwrap_or(false));
        return Ok(OutlineReport {
            path: relative_path(&path),
            text,
            truncated,
        });
    }

    if !p.is_file() {
        return Err(tool_error(format!(
            "path does not exist: {}",
            relative_path(&path)
        )));
    }

    let content = std::fs::read_to_string(p).map_err(|e| tool_error(format!("read error: {e}")))?;
    _code.record_read(p);

    let Some(lang) = LangId::from_path(p) else {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("file");
        return Ok(OutlineReport {
            path: relative_path(&path),
            text: format!("{name}: unsupported language"),
            truncated: false,
        });
    };

    let symbols = extract_symbols(&content, lang);
    let tree = build_outline_tree(&symbols);
    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let (text, truncated) = render_file_outline(name, &tree, lang);

    Ok(OutlineReport {
        path: relative_path(&path),
        text,
        truncated,
    })
}

fn outline_dir(path: &str, files_mode: bool) -> (String, bool) {
    let mut entries: Vec<DirEntry> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    let mut total_bytes: usize = 0;

    for entry in walk_source_files(path) {
        let p = Path::new(&entry);
        let content = match std::fs::read_to_string(p) {
            Ok(c) => c,
            Err(_) => {
                skipped.push(relative_path(&entry));
                continue;
            }
        };
        total_bytes += content.len();

        let Some(lang) = LangId::from_path(p) else {
            if content.len() > MAX_FILE_BYTES {
                skipped.push(format!("{} (too large)", relative_path(&entry)));
            } else {
                skipped.push(format!("{} (unsupported)", relative_path(&entry)));
            }
            continue;
        };

        if content.len() > MAX_FILE_BYTES {
            skipped.push(format!("{} (too large)", relative_path(&entry)));
            continue;
        }

        let symbols = extract_symbols(&content, lang);
        let tree = build_outline_tree(&symbols);
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("file");
        entries.push(DirEntry {
            rel_path: relative_path(&entry),
            name: name.to_string(),
            lang,
            symbol_count: count_leaves(&tree),
            bytes: content.len(),
            tree,
        });
    }

    entries.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));

    if files_mode {
        render_files_table(&entries, &skipped)
    } else {
        render_dir_outline(&entries, &skipped, total_bytes)
    }
}

const MAX_FILE_BYTES: usize = 1_000_000;

fn walk_source_files(dir: &str) -> Vec<String> {
    let mut files = Vec::new();
    let walker = ignore::WalkBuilder::new(dir)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .build();

    for entry in walker.flatten() {
        if entry.file_type().is_some_and(|ft| ft.is_file()) {
            files.push(entry.path().to_string_lossy().into_owned());
        }
    }
    files
}
