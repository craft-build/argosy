//! Unit tests for tag extraction.

use super::*;

const RUST_SAMPLE: &str = r#"
use std::sync::Arc;

pub fn hello() -> u32 { 42 }
fn helper() {}

struct Foo { x: u32 }
enum Bar { A, B }

impl Foo {
    pub fn method(&self) -> u32 { self.x }
}

fn main() {
    let f = Foo { x: 1 };
    hello();
    f.method();
}
"#;

#[test]
fn rust_tags_extract_defs_and_refs() {
    let tags = extract_tags(RUST_SAMPLE, LangId::Rust, "test.rs");
    let defs: Vec<&str> = tags
        .iter()
        .filter(|t| t.kind == TagKind::Def)
        .map(|t| t.ident.as_str())
        .collect();
    assert!(defs.contains(&"hello"));
    assert!(defs.contains(&"Foo"));
    assert!(defs.contains(&"Bar"));
    assert!(defs.contains(&"main"));
}

#[test]
fn python_tags_extract_defs() {
    let src = "def foo():\n    pass\nclass Bar:\n    def baz(self):\n        pass\n";
    let tags = extract_tags(src, LangId::Python, "test.py");
    let def_names: Vec<&str> = tags
        .iter()
        .filter(|t| t.kind == TagKind::Def)
        .map(|t| t.ident.as_str())
        .collect();
    assert!(def_names.contains(&"foo"));
    assert!(def_names.contains(&"Bar"));
    assert!(def_names.contains(&"baz"));
}

#[test]
fn sql_tags_extract_ddl_definitions() {
    let src = r#"
CREATE TABLE public.users (id INT PRIMARY KEY);
CREATE VIEW active_users AS SELECT id FROM users;
CREATE FUNCTION add_one(x INT) RETURNS INT LANGUAGE plpgsql AS $$ BEGIN RETURN x + 1; END; $$;
CREATE TRIGGER set_updated_at BEFORE UPDATE ON users EXECUTE FUNCTION update_timestamp();
CREATE INDEX idx_users_email ON users (email);
CREATE TYPE mood AS ENUM ('sad', 'ok');
CREATE SCHEMA analytics;
SELECT * FROM users;
INSERT INTO users VALUES (1);
"#;
    let tags = extract_tags(src, LangId::Sql, "schema.sql");
    let defs: Vec<&str> = tags
        .iter()
        .filter(|t| t.kind == TagKind::Def)
        .map(|t| t.ident.as_str())
        .collect();
    assert!(defs.contains(&"users"));
    assert!(defs.contains(&"active_users"));
    assert!(defs.contains(&"add_one"));
    assert!(defs.contains(&"mood"));
    assert!(defs.contains(&"analytics"));
}

#[test]
fn mentioned_idents_extracts_tokens() {
    let idents = extract_mentioned_idents("please look at the hello function and Foo struct");
    let set: std::collections::HashSet<&str> = idents.iter().map(|s| s.as_str()).collect();
    assert!(set.contains("hello"));
    assert!(set.contains("Foo"));
    assert!(set.contains("function"));
    assert!(!set.contains("at"));
}

#[test]
fn file_path_to_idents_stem_and_parts() {
    let idents = file_path_to_idents("src/foo_bar.rs");
    let set: std::collections::HashSet<&str> = idents.iter().map(|s| s.as_str()).collect();
    assert!(set.contains("foo_bar"));
    assert!(set.contains("foo"));
    assert!(set.contains("bar"));
}
