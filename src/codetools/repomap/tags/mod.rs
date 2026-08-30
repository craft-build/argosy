use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::SystemTime;

use regex::Regex;
use tracing::warn;
use tree_sitter::{Language, Parser, QueryCursor, StreamingIterator};

const MAX_FILE_BYTES: usize = 1_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TagKind {
    Def,
    Ref,
}

#[derive(Debug, Clone)]
pub struct Tag {
    pub rel_path: String,
    pub ident: String,
    pub kind: TagKind,
    pub line: usize,
}

#[derive(Debug, Clone)]
pub struct FileTags {
    pub rel_path: String,
    pub tags: Vec<Tag>,
    pub mtime: Option<SystemTime>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LangId {
    Rust,
    TypeScript,
    Python,
    Go,
    Java,
    C,
    Cpp,
    Ruby,
    Lua,
    Bash,
    Kotlin,
    Swift,
    CSharp,
    Elixir,
    Scala,
    Php,
    Html,
    Gleam,
    Dart,
    Starlark,
    Nix,
    Zig,
    Css,
    Fish,
    Perl,
    Sql,
}

impl LangId {
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext {
            "rs" => Some(Self::Rust),
            "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => Some(Self::TypeScript),
            "py" | "pyi" => Some(Self::Python),
            "go" => Some(Self::Go),
            "java" => Some(Self::Java),
            "c" | "h" => Some(Self::C),
            "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" | "ixx" => Some(Self::Cpp),
            "rb" => Some(Self::Ruby),
            "lua" => Some(Self::Lua),
            "sh" | "bash" => Some(Self::Bash),
            "kt" | "kts" => Some(Self::Kotlin),
            "swift" => Some(Self::Swift),
            "cs" => Some(Self::CSharp),
            "ex" | "exs" => Some(Self::Elixir),
            "scala" => Some(Self::Scala),
            "php" => Some(Self::Php),
            "html" | "htm" => Some(Self::Html),
            "gleam" => Some(Self::Gleam),
            "dart" => Some(Self::Dart),
            "bzl" | "bazel" | "build" => Some(Self::Starlark),
            "nix" => Some(Self::Nix),
            "zig" => Some(Self::Zig),
            "css" => Some(Self::Css),
            "fish" => Some(Self::Fish),
            "perl" => Some(Self::Perl),
            "sql" => Some(Self::Sql),
            _ => None,
        }
    }

    pub fn ts_language(&self) -> Language {
        match self {
            Self::Rust => tree_sitter_rust::LANGUAGE.into(),
            Self::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Self::Python => tree_sitter_python::LANGUAGE.into(),
            Self::Go => tree_sitter_go::LANGUAGE.into(),
            Self::Java => tree_sitter_java::LANGUAGE.into(),
            Self::C => tree_sitter_c::LANGUAGE.into(),
            Self::Cpp => tree_sitter_cpp::LANGUAGE.into(),
            Self::Ruby => tree_sitter_ruby::LANGUAGE.into(),
            Self::Lua => tree_sitter_lua::LANGUAGE.into(),
            Self::Bash => tree_sitter_bash::LANGUAGE.into(),
            Self::Kotlin => tree_sitter_kotlin_ng::LANGUAGE.into(),
            Self::Swift => tree_sitter_swift::LANGUAGE.into(),
            Self::CSharp => tree_sitter_c_sharp::LANGUAGE.into(),
            Self::Elixir => tree_sitter_elixir::LANGUAGE.into(),
            Self::Scala => tree_sitter_scala::LANGUAGE.into(),
            Self::Php => tree_sitter_php::LANGUAGE_PHP.into(),
            Self::Html => tree_sitter_html::LANGUAGE.into(),
            Self::Gleam => tree_sitter_gleam::LANGUAGE.into(),
            Self::Dart => tree_sitter_dart::LANGUAGE.into(),
            Self::Starlark => tree_sitter_starlark::LANGUAGE.into(),
            Self::Nix => tree_sitter_nix::LANGUAGE.into(),
            Self::Zig => tree_sitter_zig::LANGUAGE.into(),
            Self::Css => tree_sitter_css::LANGUAGE.into(),
            Self::Fish => tree_sitter_fish::language(),
            Self::Perl => tree_sitter_perl::LANGUAGE.into(),
            Self::Sql => tree_sitter_sequel::LANGUAGE.into(),
        }
    }
}

fn ident_regex() -> &'static Regex {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\b[a-zA-Z_][a-zA-Z0-9_]{2,}\b").unwrap());
    &RE
}

pub fn extract_tags(content: &str, lang: LangId, rel_path: &str) -> Vec<Tag> {
    let mut parser = Parser::new();
    if parser.set_language(&lang.ts_language()).is_err() {
        warn!("repomap parser rejected language abi");
        return vec![];
    }
    let Some(tree) = parser.parse(content, None) else {
        return vec![];
    };
    let query = match tags_query(lang) {
        Some(q) => q,
        None => return vec![],
    };

    let root = tree.root_node();
    let mut cursor = QueryCursor::new();
    cursor.set_match_limit(65536);
    let mut matches = cursor.matches(query, root, content.as_bytes());

    let mut tags = Vec::new();
    let mut seen: std::collections::HashSet<(TagKind, usize, String)> =
        std::collections::HashSet::new();

    while let Some(m) = matches.next() {
        for cap in m.captures {
            let names = query.capture_names();
            let capture_name = names[cap.index as usize];
            let node = cap.node;
            let kind = match capture_name {
                n if n.starts_with("name.definition") => TagKind::Def,
                n if n.starts_with("name.reference") => TagKind::Ref,
                _ => continue,
            };

            let ident = content[node.byte_range()].trim().to_string();
            if ident.is_empty() {
                continue;
            }

            let line = node.start_position().row + 1;
            let key = (kind, line, ident.clone());
            if !seen.insert(key) {
                continue;
            }

            tags.push(Tag {
                rel_path: rel_path.to_string(),
                ident,
                kind,
                line,
            });
        }
    }

    let ref_tags: Vec<Tag> = tags
        .iter()
        .filter(|t| t.kind == TagKind::Ref)
        .map(|t| t.ident.clone())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .map(|ident| Tag {
            rel_path: rel_path.to_string(),
            ident,
            kind: TagKind::Ref,
            line: 0,
        })
        .collect();

    let mut def_tags: Vec<Tag> = tags
        .into_iter()
        .filter(|t| t.kind == TagKind::Def)
        .collect();
    def_tags.extend(ref_tags);
    def_tags
}

fn walk_tracked_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let walker = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(true)
        .build();

    for entry in walker.flatten() {
        if entry.file_type().is_some_and(|ft| ft.is_file())
            && let Some(ext) = entry.path().extension().and_then(|e| e.to_str())
            && LangId::from_extension(ext).is_some()
        {
            files.push(entry.path().to_path_buf());
        }
    }
    files
}

pub fn collect_all_tags(root: &Path) -> Vec<FileTags> {
    let mut result = Vec::new();
    for path in walk_tracked_files(root) {
        let rel = match path.strip_prefix(root) {
            Ok(r) => r.to_string_lossy().to_string(),
            Err(_) => continue,
        };
        let lang = match path
            .extension()
            .and_then(|e| e.to_str())
            .and_then(LangId::from_extension)
        {
            Some(l) => l,
            None => continue,
        };
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        if content.len() > MAX_FILE_BYTES {
            continue;
        }
        let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        let tags = extract_tags(&content, lang, &rel);
        if !tags.is_empty() {
            result.push(FileTags {
                rel_path: rel,
                tags,
                mtime,
            });
        }
    }
    result
}

pub fn extract_mentioned_idents(text: &str) -> Vec<String> {
    ident_regex()
        .find_iter(text)
        .map(|m| m.as_str().to_string())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect()
}

pub fn file_path_to_idents(rel_path: &str) -> Vec<String> {
    let stem = Path::new(rel_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    let mut idents = vec![stem.to_string()];
    let parts: Vec<&str> = stem
        .split(['_', '-', '.'])
        .filter(|s| !s.is_empty())
        .collect();
    for part in parts {
        if part != stem {
            idents.push(part.to_string());
        }
    }
    idents
}

mod queries;

#[cfg(test)]
mod tests;

use queries::tags_query;
