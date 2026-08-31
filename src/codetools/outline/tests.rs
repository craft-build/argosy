//! Unit tests: extraction, tree building, rendering, and the language
//! registry.

use tree_sitter::Query;

use super::extract::{TOML_FIELD_TRUNCATE_THRESHOLD, extract_symbols};
use super::lang::LangId;
use super::queries::{ALL_LANGS, query_source};
use super::render::{build_outline_tree, render_file_outline, truncate_signature, truncate_outline};
use super::types::{Symbol, SymbolKind};

use super::*;

const RUST_SRC: &str = r#"
use std::fs;

pub struct Config {
    name: String,
}

impl Config {
    pub fn new() -> Self {
        Self { name: String::new() }
    }
}

fn main() {
    let config = Config::new();
}
"#;

#[test]
fn rust_outline_extracts_struct_and_fn() {
    let symbols = extract_symbols(RUST_SRC, LangId::Rust);
    assert!(
        symbols
            .iter()
            .any(|s| s.name == "Config" && s.kind == SymbolKind::Struct)
    );
    assert!(
        symbols
            .iter()
            .any(|s| s.name == "new" && s.kind == SymbolKind::Method)
    );
    assert!(
        symbols
            .iter()
            .any(|s| s.name == "main" && s.kind == SymbolKind::Function)
    );
}

#[test]
fn rust_outline_builds_tree() {
    let symbols = extract_symbols(RUST_SRC, LangId::Rust);
    let tree = build_outline_tree(&symbols);
    assert!(
        tree.iter()
            .any(|e| e.name == "Config" && !e.members.is_empty())
    );
}

#[test]
fn rust_outline_renders() {
    let symbols = extract_symbols(RUST_SRC, LangId::Rust);
    let tree = build_outline_tree(&symbols);
    let (text, truncated) = render_file_outline("main.rs", &tree, LangId::Rust);
    assert!(!truncated);
    assert!(
        text.contains("main.rs"),
        "missing 'main.rs' in output:\n{text}"
    );
    assert!(
        text.contains("Config"),
        "missing 'Config' in output:\n{text}"
    );
    assert!(text.contains("main"), "missing 'main' in output:\n{text}");
}

#[test]
fn lang_from_extension() {
    assert_eq!(LangId::from_extension("rs"), Some(LangId::Rust));
    assert_eq!(LangId::from_extension("py"), Some(LangId::Python));
    assert_eq!(LangId::from_extension("txt"), None);
}

#[test]
fn truncate_signature_long() {
    let sig = "fn very_long_function_name(with: many, arguments: that, make: it, exceed: the, limit: of, eighty: characters) -> Result<Type, Error>";
    let truncated = truncate_signature(sig);
    assert!(truncated.chars().count() <= 81);
    assert!(truncated.ends_with('…'));
}

/// The byte cut must land on a char boundary: outline text routinely
/// contains multibyte characters, and `String::truncate` panics when the
/// new length splits one.
#[test]
fn outline_truncation_lands_on_char_boundaries() {
    // Every `é` is two bytes, so a byte cut at an odd offset would split
    // a character — exactly the case at the 30KB cap.
    let mut out = "é".repeat(20_000);
    assert!(out.len() > 30_000);
    let (text, truncated) = truncate_outline(&mut out);
    assert!(truncated);
    assert!(std::str::from_utf8(text.as_bytes()).is_ok());
    assert!(text.ends_with("(output truncated, narrow the path to see more)"));
    assert!(text.len() <= 30_000);
}

/// A markdown heading's range must cover its whole section, not just the
/// heading line: zoom promises "section content under a heading".
#[test]
fn markdown_heading_range_covers_its_section() {
    let src = "# Title\n\nintro\n\n## Setup\n\nstep one\nstep two\n\n## Usage\n\nuse it\n\n# Next\n";
    let symbols = extract_symbols(src, LangId::Markdown);
    let setup = symbols.iter().find(|s| s.name == "Setup").unwrap();
    assert_eq!(setup.range.start_row, 4);
    // Runs until the `## Usage` heading (row 9), exclusive.
    assert_eq!(setup.range.end_row, 8);

    let title = symbols.iter().find(|s| s.name == "Title").unwrap();
    // An h1 section ends only at the next h1 — `# Next` on row 13.
    assert_eq!(title.range.end_row, 12);

    let next = symbols.iter().find(|s| s.name == "Next").unwrap();
    // The last heading runs to EOF.
    assert_eq!(next.range.end_row, 13);
}

/// Only h1-h6 are headings in HTML-family grammars; the raw query captures
/// every element.
#[test]
fn html_outline_only_takes_h1_to_h6_headings() {
    let src =
        "<html><body><h1>Title</h1><div>content</div><span>x</span><h2>Sub</h2></body></html>\n";
    let symbols = extract_symbols(src, LangId::Html);
    let names: Vec<_> = symbols.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["h1", "h2"]);
}

#[test]
fn python_outline_extracts_class_and_fn() {
    let src = "class Foo:\n    def bar(self):\n        pass\n\ndef baz():\n    pass\n";
    let symbols = extract_symbols(src, LangId::Python);
    assert!(
        symbols
            .iter()
            .any(|s| s.name == "Foo" && s.kind == SymbolKind::Class)
    );
    assert!(
        symbols
            .iter()
            .any(|s| s.name == "bar" && s.kind == SymbolKind::Method)
    );
    assert!(
        symbols
            .iter()
            .any(|s| s.name == "baz" && s.kind == SymbolKind::Function)
    );
}

#[test]
fn go_outline_extracts_fn_method_type_and_alias() {
    let src = "\
package main

import \"fmt\"

func foo() {}

func (r T) bar() {}

type Struct struct{ a int }
type Alias = int
";
    let symbols = extract_symbols(src, LangId::Go);
    assert!(
        symbols
            .iter()
            .any(|s| s.name == "foo" && s.kind == SymbolKind::Function)
    );
    assert!(
        symbols
            .iter()
            .any(|s| s.name == "bar" && s.kind == SymbolKind::Method)
    );
    assert!(
        symbols
            .iter()
            .any(|s| s.name == "Struct" && s.kind == SymbolKind::TypeAlias)
    );
    assert!(
        symbols
            .iter()
            .any(|s| s.name == "Alias" && s.kind == SymbolKind::TypeAlias)
    );
    assert!(symbols.iter().any(|s| s.kind == SymbolKind::Import));
}

#[test]
fn all_queries_compile_against_grammar() {
    for lang in ALL_LANGS {
        let src = query_source(*lang);
        let result = Query::new(&lang.ts_language(), src);
        assert!(
            result.is_ok(),
            "{} query failed to compile against installed grammar: {:?}",
            lang.name(),
            result.err()
        );
    }
}

#[test]
fn broken_query_degrades_gracefully_without_panic() {
    let symbols = extract_symbols("garbage :: source", LangId::Nix);
    assert!(symbols.iter().all(|s| s.kind == SymbolKind::Variable));
}

#[test]
fn nix_outline_extracts_bindings() {
    let src = "{ a = 1; b.c = 2; }";
    let symbols = extract_symbols(src, LangId::Nix);
    let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"a"), "expected binding a, got {names:?}");
    assert!(
        names.iter().any(|n| n.contains("b")),
        "expected binding b.c, got {names:?}"
    );
}

#[test]
fn typescript_outline_extracts_var_declarator() {
    let src = "const x = 1;\nlet y: number = 2;\nfunction foo() {}\nclass Bar {}\n";
    let symbols = extract_symbols(src, LangId::TypeScript);
    assert!(
        symbols
            .iter()
            .any(|s| s.name == "x" && s.kind == SymbolKind::Variable)
    );
    assert!(
        symbols
            .iter()
            .any(|s| s.name == "y" && s.kind == SymbolKind::Variable)
    );
    assert!(
        symbols
            .iter()
            .any(|s| s.name == "foo" && s.kind == SymbolKind::Function)
    );
}

#[test]
fn sql_outline_extracts_ddl_definitions() {
    let src = "\
CREATE TABLE public.users (id INT PRIMARY KEY, name VARCHAR(255));
CREATE VIEW active_users AS SELECT id FROM users;
CREATE MATERIALIZED VIEW mv_totals AS SELECT sum(amount) FROM orders;
CREATE FUNCTION add_one(x INT) RETURNS INT LANGUAGE plpgsql AS $$ BEGIN RETURN x + 1; END; $$;
CREATE TRIGGER set_updated_at BEFORE UPDATE ON users FOR EACH ROW EXECUTE FUNCTION update_timestamp();
CREATE INDEX idx_users_email ON users (email);
CREATE TYPE mood AS ENUM ('sad', 'ok');
CREATE SCHEMA analytics;
";
    let symbols = extract_symbols(src, LangId::Sql);
    let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"public.users"), "table name: {names:?}");
    assert!(names.contains(&"active_users"), "view name: {names:?}");
    assert!(names.contains(&"mv_totals"), "materialized view: {names:?}");
    assert!(names.contains(&"add_one"), "function name: {names:?}");
    assert!(
        symbols
            .iter()
            .any(|s| s.name == "set_updated_at" && s.kind == SymbolKind::Function),
        "trigger name should win over table/callee: {names:?}"
    );
    assert!(names.contains(&"idx_users_email"), "index name: {names:?}");
    assert!(names.contains(&"mood"), "type name: {names:?}");
    assert!(names.contains(&"analytics"), "schema name: {names:?}");

    let tree = build_outline_tree(&symbols);
    let users = tree
        .iter()
        .find(|e| e.name == "public.users")
        .expect("users table entry");
    let cols: Vec<&str> = users.members.iter().map(|m| m.name.as_str()).collect();
    assert!(
        cols.contains(&"id"),
        "table columns nest as members: {cols:?}"
    );
    assert!(cols.contains(&"name"));
}

#[test]
fn sql_outline_skips_dml_and_alter() {
    let src = "\
SELECT * FROM users;
INSERT INTO users (id) VALUES (1);
UPDATE users SET name = 'x' WHERE id = 1;
DELETE FROM users WHERE id = 1;
ALTER TABLE users ADD COLUMN age INT;
DROP TABLE users;
";
    let symbols = extract_symbols(src, LangId::Sql);
    assert!(
        symbols.is_empty(),
        "DML/ALTER/DROP must yield no symbols, got {symbols:?}"
    );
}

#[test]
fn yaml_outline_extracts_top_level_keys() {
    let src = "name: craft\nversion: \"0.9.4\"\ndescription: agent\n";
    let symbols = extract_symbols(src, LangId::Yaml);
    let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"name"));
    assert!(names.contains(&"version"));
    assert!(names.contains(&"description"));
    assert!(symbols.iter().all(|s| s.kind == SymbolKind::Constant));
}

#[test]
fn yaml_outline_nests_one_level_of_children() {
    let src = "\
services:
  web:
    image: nginx
  db:
    image: postgres
";
    let symbols = extract_symbols(src, LangId::Yaml);
    let tree = build_outline_tree(&symbols);
    let services = tree
        .iter()
        .find(|e| e.name == "services")
        .expect("services root");
    let member_names: Vec<&str> = services.members.iter().map(|m| m.name.as_str()).collect();
    assert!(member_names.contains(&"web"));
    assert!(member_names.contains(&"db"));
    assert!(
        !member_names.contains(&"image"),
        "depth-2 keys must not be indexed, got {member_names:?}"
    );
}

#[test]
fn yaml_outline_unwraps_sequence_of_mappings() {
    let src = "\
items:
  - name: first
    value: 1
  - name: second
    value: 2
";
    let symbols = extract_symbols(src, LangId::Yaml);
    let tree = build_outline_tree(&symbols);
    let items = tree.iter().find(|e| e.name == "items").expect("items root");
    let member_names: Vec<&str> = items.members.iter().map(|m| m.name.as_str()).collect();
    assert!(member_names.contains(&"name"));
    assert!(member_names.contains(&"value"));
    assert!(
        !member_names.contains(&"first"),
        "scalar values must not be indexed, got {member_names:?}"
    );
}

#[test]
fn yaml_outline_skips_scalar_sequence_values() {
    let src = "ports:\n  - 8080\n  - 8443\nenv:\n  - FOO=1\n";
    let symbols = extract_symbols(src, LangId::Yaml);
    let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"ports"));
    assert!(names.contains(&"env"));
    assert!(
        !names
            .iter()
            .any(|n| n.contains("8080") || n.contains("FOO")),
        "sequence scalars must not be indexed, got {names:?}"
    );
}

#[test]
fn yaml_outline_handles_multi_document_stream() {
    let src = "---\ntitle: first\n---\ntitle: second\n";
    let symbols = extract_symbols(src, LangId::Yaml);
    let titles = symbols.iter().filter(|s| s.name == "title").count();
    assert_eq!(titles, 2, "expected a title from each document");
}

#[test]
fn yaml_outline_strips_quotes_from_keys() {
    let src = "\"full name\": craft\n'machine': x86\n";
    let symbols = extract_symbols(src, LangId::Yaml);
    let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"full name"));
    assert!(names.contains(&"machine"));
}

#[test]
fn yaml_outline_scalar_only_document_yields_nothing() {
    let symbols = extract_symbols("just a scalar\n", LangId::Yaml);
    assert!(
        symbols.is_empty(),
        "scalar-only document must yield no symbols"
    );
}

#[test]
fn yaml_outline_renders() {
    let src = "name: craft\nservices:\n  web:\n    image: nginx\n";
    let symbols = extract_symbols(src, LangId::Yaml);
    let tree = build_outline_tree(&symbols);
    let (text, _) = render_file_outline("compose.yaml", &tree, LangId::Yaml);
    assert!(
        text.contains("compose.yaml"),
        "missing 'compose.yaml' in output:\n{text}"
    );
    assert!(text.contains("name"), "missing 'name' in output:\n{text}");
    assert!(
        text.contains("services"),
        "missing 'services' in output:\n{text}"
    );
}

#[test]
fn toml_outline_extracts_top_level_pairs() {
    let src = "title = \"TOML Example\"\nversion = 1\n";
    let symbols = extract_symbols(src, LangId::Toml);
    let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"title"));
    assert!(names.contains(&"version"));
    assert!(symbols.iter().all(|s| s.kind == SymbolKind::Constant));
    let title = symbols.iter().find(|s| s.name == "title").expect("title");
    assert!(
        title
            .signature
            .as_deref()
            .unwrap_or("")
            .contains("TOML Example"),
        "top-level pair should keep value in signature, got {:?}",
        title.signature
    );
}

#[test]
fn toml_outline_renders_table_header_and_pairs() {
    let src = "[package]\nname = \"craft\"\nversion = \"0.9.5\"\n";
    let symbols = extract_symbols(src, LangId::Toml);
    let tree = build_outline_tree(&symbols);
    let (text, _) = render_file_outline("Cargo.toml", &tree, LangId::Toml);
    assert!(
        text.contains("Cargo.toml"),
        "missing 'Cargo.toml' in output:\n{text}"
    );
    assert!(
        text.contains("[package]"),
        "missing '[package]' in output:\n{text}"
    );
    assert!(text.contains("name"), "missing 'name' in output:\n{text}");
    assert!(
        text.contains("version"),
        "missing 'version' in output:\n{text}"
    );
    let package = tree
        .iter()
        .find(|e| e.name == "[package]")
        .expect("[package] root");
    let member_names: Vec<&str> = package.members.iter().map(|m| m.name.as_str()).collect();
    assert!(member_names.contains(&"name"));
    assert!(member_names.contains(&"version"));
}

#[test]
fn toml_outline_handles_table_array_elements() {
    let src = "[[bin]]\nname = \"craft\"\npath = \"src/main.rs\"\n";
    let symbols = extract_symbols(src, LangId::Toml);
    let tree = build_outline_tree(&symbols);
    let bin = tree
        .iter()
        .find(|e| e.name == "[[bin]]")
        .expect("[[bin]] root");
    let member_names: Vec<&str> = bin.members.iter().map(|m| m.name.as_str()).collect();
    assert!(member_names.contains(&"name"));
    assert!(member_names.contains(&"path"));
}

#[test]
fn toml_outline_keeps_dotted_and_quoted_keys() {
    let src = "a.b.c = 1\n[\"quoted.section\"]\n\"weird.key\" = 1\n";
    let symbols = extract_symbols(src, LangId::Toml);
    let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"a.b.c"));
    assert!(names.contains(&"[\"quoted.section\"]"));
    assert!(names.contains(&"\"weird.key\""));
}

#[test]
fn toml_outline_truncates_pairs_past_threshold() {
    let mut src = "[data]\n".to_string();
    for i in 1..=9 {
        src.push_str(&format!("k{i} = {i}\n"));
    }
    let symbols = extract_symbols(&src, LangId::Toml);
    let pairs: Vec<&Symbol> = symbols
        .iter()
        .filter(|s| s.scope_chain == vec!["[data]".to_string()])
        .collect();
    assert_eq!(pairs.len(), 9);
    let with_value = pairs.iter().filter(|s| s.signature.is_some()).count();
    assert_eq!(
        with_value, TOML_FIELD_TRUNCATE_THRESHOLD,
        "first {} pairs should keep their value, got {with_value}",
        TOML_FIELD_TRUNCATE_THRESHOLD
    );
    let k9 = pairs.iter().find(|s| s.name == "k9").expect("k9 pair");
    assert!(
        k9.signature.is_none(),
        "9th pair should drop value past threshold, got {:?}",
        k9.signature
    );
}

#[test]
fn toml_outline_ignores_comments() {
    let src = "# top comment\n[server]\n# inline comment\nhost = \"localhost\" # trailing\n";
    let symbols = extract_symbols(src, LangId::Toml);
    let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"[server]"));
    assert!(names.contains(&"host"));
    assert!(
        !symbols
            .iter()
            .any(|s| s.name.contains("comment") || s.name.contains("trailing")),
        "comments must not become symbols, got {symbols:?}"
    );
}

#[test]
fn toml_outline_empty_table_keeps_header() {
    let src = "top = \"value\"\n[empty]\n[next]\nx = 1\n";
    let symbols = extract_symbols(src, LangId::Toml);
    let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"top"));
    assert!(names.contains(&"[empty]"));
    assert!(names.contains(&"[next]"));
}

#[test]
fn run_reports_unsupported_language_without_error() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("data.txt");
    std::fs::write(&file, "plain text").unwrap();

    let tools = super::CodeTools::default();
    let report = run(
        &tools,
        OutlineParams {
            path: file.to_string_lossy().into_owned(),
            files: None,
        },
    )
    .unwrap();
    assert_eq!(report.text, "data.txt: unsupported language");
    assert!(!report.truncated);
}

#[test]
fn run_outlines_directory_with_skips() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(dir.path().join("notes.txt"), "not code\n").unwrap();

    let tools = super::CodeTools::default();
    let report = run(
        &tools,
        OutlineParams {
            path: dir.path().to_string_lossy().into_owned(),
            files: None,
        },
    )
    .unwrap();
    assert!(report.text.contains("main.rs"), "got {}", report.text);
    assert!(report.text.contains("fn main"), "got {}", report.text);
    assert!(report.text.contains("notes.txt (unsupported)"));
    assert!(report.text.contains("total: 1 files"));
}

#[test]
fn run_missing_path_errors() {
    let tools = super::CodeTools::default();
    let err = run(
        &tools,
        OutlineParams {
            path: "/definitely/not/here.rs".into(),
            files: None,
        },
    )
    .unwrap_err();
    assert!(err.to_string().contains("does not exist"), "{err}");
}

/// Directory mode records its reads like every other code tool, so the
/// stale-read guard covers files an agent last saw through an outline.
#[test]
fn run_dir_mode_records_reads_for_the_stale_guard() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "fn a() {}\n").unwrap();

    let tools = super::CodeTools::default();
    let report = run(
        &tools,
        OutlineParams {
            path: dir.path().to_string_lossy().into_owned(),
            files: None,
        },
    )
    .unwrap();
    assert!(report.text.contains("a.rs"), "got {}", report.text);

    let file = dir.path().join("a.rs");
    std::fs::write(&file, "fn a() { changed }\n").unwrap();
    assert!(
        tools.check_before_edit(std::path::Path::new(&file)).is_err(),
        "the outline's read must feed the stale-read guard"
    );
}

#[test]
fn run_dir_mode_labels_oversize_files_skipped() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("big.rs"), "x".repeat(MAX_FILE_BYTES + 10)).unwrap();
    std::fs::write(dir.path().join("ok.rs"), "fn ok() {}\n").unwrap();

    let tools = super::CodeTools::default();
    let report = run(
        &tools,
        OutlineParams {
            path: dir.path().to_string_lossy().into_owned(),
            files: Some(true),
        },
    )
    .unwrap();
    assert!(report.text.contains("big.rs (too large)"), "got {}", report.text);
    assert!(report.text.contains("ok.rs"));
}

#[test]
fn run_single_oversize_file_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let big = dir.path().join("big.rs");
    std::fs::write(&big, "x".repeat(MAX_FILE_BYTES + 1)).unwrap();

    let tools = super::CodeTools::default();
    let err = run(
        &tools,
        OutlineParams {
            path: big.to_string_lossy().into_owned(),
            files: None,
        },
    )
    .unwrap_err();
    assert!(err.to_string().contains("too large"), "{err}");
}

/// A Rust function is a method only inside `impl`/`trait` — a named
/// enclosing scope alone (`mod tests`) must not relabel it.
#[test]
fn rust_functions_inside_mods_stay_functions() {
    let src = "mod tests {\n    fn helper() {}\n}\nimpl Foo {\n    fn method(&self) {}\n}\nfn free() {}\n";
    let symbols = extract_symbols(src, LangId::Rust);
    let helper = symbols.iter().find(|s| s.name == "helper").unwrap();
    assert_eq!(helper.kind, SymbolKind::Function);
    let method = symbols.iter().find(|s| s.name == "method").unwrap();
    assert_eq!(method.kind, SymbolKind::Method);
    let free = symbols.iter().find(|s| s.name == "free").unwrap();
    assert_eq!(free.kind, SymbolKind::Function);
}
