//! Per-language tree-sitter tag queries and their compiled caches.

use std::sync::LazyLock;

use tracing::warn;
use tree_sitter::{Language, Query};

use super::LangId;

pub(super) fn tags_query(lang: LangId) -> Option<&'static Query> {
    lang_tags_query(lang).as_ref()
}

fn build_tags_query(lang_name: &str, language: &Language, src: &'static str) -> Option<Query> {
    match Query::new(language, src) {
        Ok(q) => Some(q),
        Err(e) => {
            warn!(error = %e, lang = lang_name, "repomap tags query failed to compile");
            None
        }
    }
}

fn lang_tags_query(lang: LangId) -> &'static LazyLock<Option<Query>> {
    match lang {
        LangId::Rust => &RUST_TAGS_QUERY,
        LangId::TypeScript => &TS_TAGS_QUERY,
        LangId::Python => &PY_TAGS_QUERY,
        LangId::Go => &GO_TAGS_QUERY,
        LangId::Java => &JAVA_TAGS_QUERY,
        LangId::C => &C_TAGS_QUERY,
        LangId::Cpp => &CPP_TAGS_QUERY,
        LangId::Ruby => &RUBY_TAGS_QUERY,
        LangId::Lua => &LUA_TAGS_QUERY,
        LangId::Bash => &BASH_TAGS_QUERY,
        LangId::Kotlin => &KT_TAGS_QUERY,
        LangId::Swift => &SWIFT_TAGS_QUERY,
        LangId::CSharp => &CSHARP_TAGS_QUERY,
        LangId::Elixir => &ELIXIR_TAGS_QUERY,
        LangId::Scala => &SCALA_TAGS_QUERY,
        LangId::Php => &PHP_TAGS_QUERY,
        LangId::Html => &HTML_TAGS_QUERY,
        LangId::Gleam => &GLEAM_TAGS_QUERY,
        LangId::Dart => &DART_TAGS_QUERY,
        LangId::Starlark => &STARLARK_TAGS_QUERY,
        LangId::Nix => &NIX_TAGS_QUERY,
        LangId::Zig => &ZIG_TAGS_QUERY,
        LangId::Css => &CSS_TAGS_QUERY,
        LangId::Fish => &FISH_TAGS_QUERY,
        LangId::Perl => &PERL_TAGS_QUERY,
        LangId::Sql => &SQL_TAGS_QUERY,
    }
}

const RUST_TAGS_SRC: &str = r#"
(function_item name: (identifier) @name.definition.function) @definition.function
(impl_item type: (type_identifier) @name.definition.class) @definition.class
(struct_item name: (type_identifier) @name.definition.class) @definition.class
(enum_item name: (type_identifier) @name.definition.class) @definition.class
(trait_item name: (type_identifier) @name.definition.class) @definition.class
(type_item name: (type_identifier) @name.definition.class) @definition.class
(const_item name: (identifier) @name.definition.constant) @definition.constant
(mod_item name: (identifier) @name.definition.module) @definition.module
(macro_definition name: (identifier) @name.definition.macro) @definition.macro
(call_expression function: (identifier) @name.reference)
(call_expression function: (field_expression field: (field_identifier) @name.reference))
(use_declaration argument: (scoped_identifier name: (identifier) @name.reference))
(use_declaration argument: (identifier) @name.reference)
"#;
static RUST_TAGS_QUERY: LazyLock<Option<Query>> =
    LazyLock::new(|| build_tags_query("rust", &tree_sitter_rust::LANGUAGE.into(), RUST_TAGS_SRC));

const TS_TAGS_SRC: &str = r#"
(function_declaration name: (identifier) @name.definition.function) @definition.function
(method_definition name: (property_identifier) @name.definition.function) @definition.function
(class_declaration name: (type_identifier) @name.definition.class) @definition.class
(interface_declaration name: (type_identifier) @name.definition.class) @definition.class
(type_alias_declaration name: (type_identifier) @name.definition.class) @definition.class
(variable_declarator name: (identifier) @name.definition.constant) @definition.constant
(enum_declaration name: (identifier) @name.definition.class) @definition.class
(call_expression function: (identifier) @name.reference)
(call_expression function: (member_expression property: (property_identifier) @name.reference))
(new_expression constructor: (identifier) @name.reference)
(type_annotation (type_identifier) @name.reference)
"#;
static TS_TAGS_QUERY: LazyLock<Option<Query>> = LazyLock::new(|| {
    build_tags_query(
        "typescript",
        &tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        TS_TAGS_SRC,
    )
});

const PY_TAGS_SRC: &str = r#"
(function_definition name: (identifier) @name.definition.function) @definition.function
(class_definition name: (identifier) @name.definition.class) @definition.class
(assignment left: (identifier) @name.definition.constant) @definition.constant
(call function: (identifier) @name.reference)
(call function: (attribute attribute: (identifier) @name.reference))
"#;
static PY_TAGS_QUERY: LazyLock<Option<Query>> =
    LazyLock::new(|| build_tags_query("python", &tree_sitter_python::LANGUAGE.into(), PY_TAGS_SRC));

const GO_TAGS_SRC: &str = r#"
(function_declaration name: (identifier) @name.definition.function) @definition.function
(method_declaration name: (field_identifier) @name.definition.function) @definition.function
(type_declaration (type_spec name: (type_identifier) @name.definition.class)) @definition.class
(type_declaration (type_alias name: (type_identifier) @name.definition.class)) @definition.class
"#;
static GO_TAGS_QUERY: LazyLock<Option<Query>> =
    LazyLock::new(|| build_tags_query("go", &tree_sitter_go::LANGUAGE.into(), GO_TAGS_SRC));

const JAVA_TAGS_SRC: &str = r#"
(class_declaration name: (identifier) @name.definition.class) @definition.class
(method_declaration name: (identifier) @name.definition.function) @definition.function
(interface_declaration name: (identifier) @name.definition.class) @definition.class
(enum_declaration name: (identifier) @name.definition.class) @definition.class
(method_invocation object: (identifier) @name.reference)
(method_invocation name: (identifier) @name.reference)
(object_creation_expression type: (type_identifier) @name.reference)
"#;
static JAVA_TAGS_QUERY: LazyLock<Option<Query>> =
    LazyLock::new(|| build_tags_query("java", &tree_sitter_java::LANGUAGE.into(), JAVA_TAGS_SRC));

const C_TAGS_SRC: &str = r#"
(function_definition declarator: (function_declarator declarator: (identifier) @name.definition.function)) @definition.function
(declaration type: (primitive_type) declarator: (identifier) @name.definition.constant) @definition.constant
(declaration type: (type_identifier) declarator: (identifier) @name.definition.constant) @definition.constant
(type_definition declarator: (type_identifier) @name.definition.class) @definition.class
(call_expression function: (identifier) @name.reference)
"#;
static C_TAGS_QUERY: LazyLock<Option<Query>> =
    LazyLock::new(|| build_tags_query("c", &tree_sitter_c::LANGUAGE.into(), C_TAGS_SRC));

const CPP_TAGS_SRC: &str = r#"
(function_definition declarator: (function_declarator declarator: (identifier) @name.definition.function)) @definition.function
(class_specifier name: (type_identifier) @name.definition.class) @definition.class
(struct_specifier name: (type_identifier) @name.definition.class) @definition.class
(declaration type: (type_identifier) declarator: (identifier) @name.definition.constant) @definition.constant
(call_expression function: (identifier) @name.reference)
"#;
static CPP_TAGS_QUERY: LazyLock<Option<Query>> =
    LazyLock::new(|| build_tags_query("cpp", &tree_sitter_cpp::LANGUAGE.into(), CPP_TAGS_SRC));

const RUBY_TAGS_SRC: &str = r#"
(class name: (constant) @name.definition.class) @definition.class
(method name: (identifier) @name.definition.function) @definition.function
(module name: (constant) @name.definition.module) @definition.module
(call method: (identifier) @name.reference)
"#;
static RUBY_TAGS_QUERY: LazyLock<Option<Query>> =
    LazyLock::new(|| build_tags_query("ruby", &tree_sitter_ruby::LANGUAGE.into(), RUBY_TAGS_SRC));

const LUA_TAGS_SRC: &str = r#"
(function_declaration name: (identifier) @name.definition.function) @definition.function
(function name: (identifier) @name.definition.function) @definition.function
(assignment (identifier) @name.definition.constant) @definition.constant
"#;
static LUA_TAGS_QUERY: LazyLock<Option<Query>> =
    LazyLock::new(|| build_tags_query("lua", &tree_sitter_lua::LANGUAGE.into(), LUA_TAGS_SRC));

const BASH_TAGS_SRC: &str = r#"
(function_definition name: (word) @name.definition.function) @definition.function
(variable_assignment name: (variable_name) @name.definition.constant) @definition.constant
"#;
static BASH_TAGS_QUERY: LazyLock<Option<Query>> =
    LazyLock::new(|| build_tags_query("bash", &tree_sitter_bash::LANGUAGE.into(), BASH_TAGS_SRC));

const KT_TAGS_SRC: &str = r#"
(class_declaration name: (type_identifier) @name.definition.class) @definition.class
(function_declaration (simple_identifier) @name.definition.function) @definition.function
(object_declaration name: (type_identifier) @name.definition.class) @definition.class
(interface_declaration name: (type_identifier) @name.definition.class) @definition.class
"#;
static KT_TAGS_QUERY: LazyLock<Option<Query>> = LazyLock::new(|| {
    build_tags_query(
        "kotlin",
        &tree_sitter_kotlin_ng::LANGUAGE.into(),
        KT_TAGS_SRC,
    )
});

const SWIFT_TAGS_SRC: &str = r#"
(function_declaration name: (simple_identifier) @name.definition.function) @definition.function
(class_declaration name: (type_identifier) @name.definition.class) @definition.class
(struct_declaration name: (type_identifier) @name.definition.class) @definition.class
(protocol_declaration name: (type_identifier) @name.definition.class) @definition.class
(enum_declaration name: (type_identifier) @name.definition.class) @definition.class
"#;
static SWIFT_TAGS_QUERY: LazyLock<Option<Query>> = LazyLock::new(|| {
    build_tags_query("swift", &tree_sitter_swift::LANGUAGE.into(), SWIFT_TAGS_SRC)
});

const CSHARP_TAGS_SRC: &str = r#"
(class_declaration name: (identifier) @name.definition.class) @definition.class
(method_declaration name: (identifier) @name.definition.function) @definition.function
(interface_declaration name: (identifier) @name.definition.class) @definition.class
(struct_declaration name: (identifier) @name.definition.class) @definition.class
(enum_declaration name: (identifier) @name.definition.class) @definition.class
(invocation_expression function: (identifier) @name.reference)
(object_creation_expression type: (identifier) @name.reference)
"#;
static CSHARP_TAGS_QUERY: LazyLock<Option<Query>> = LazyLock::new(|| {
    build_tags_query(
        "csharp",
        &tree_sitter_c_sharp::LANGUAGE.into(),
        CSHARP_TAGS_SRC,
    )
});

const ELIXIR_TAGS_SRC: &str = r#"
(call target: (identifier) @ignore)
(unary_operator operand: (call target: (identifier) @name.definition.function)) @definition.function
"#;
static ELIXIR_TAGS_QUERY: LazyLock<Option<Query>> = LazyLock::new(|| {
    build_tags_query(
        "elixir",
        &tree_sitter_elixir::LANGUAGE.into(),
        ELIXIR_TAGS_SRC,
    )
});

const SCALA_TAGS_SRC: &str = r#"
(class_definition name: (identifier) @name.definition.class) @definition.class
(object_definition name: (identifier) @name.definition.class) @definition.class
(trait_definition name: (identifier) @name.definition.class) @definition.class
(function_definition name: (identifier) @name.definition.function) @definition.function
"#;
static SCALA_TAGS_QUERY: LazyLock<Option<Query>> = LazyLock::new(|| {
    build_tags_query("scala", &tree_sitter_scala::LANGUAGE.into(), SCALA_TAGS_SRC)
});

const PHP_TAGS_SRC: &str = r#"
(function_definition name: (name) @name.definition.function) @definition.function
(class_declaration name: (name) @name.definition.class) @definition.class
(interface_declaration name: (name) @name.definition.class) @definition.class
(method_declaration name: (name) @name.definition.function) @definition.function
(function_call_expression function: (name) @name.reference)
"#;
static PHP_TAGS_QUERY: LazyLock<Option<Query>> =
    LazyLock::new(|| build_tags_query("php", &tree_sitter_php::LANGUAGE_PHP.into(), PHP_TAGS_SRC));

const HTML_TAGS_SRC: &str = r#"
(element (start_tag (tag_name) @name.definition.class)) @definition.class
"#;
static HTML_TAGS_QUERY: LazyLock<Option<Query>> =
    LazyLock::new(|| build_tags_query("html", &tree_sitter_html::LANGUAGE.into(), HTML_TAGS_SRC));

const GLEAM_TAGS_SRC: &str = r#"
(function_definition name: (identifier) @name.definition.function) @definition.function
(custom_type_definition name: (type_identifier) @name.definition.class) @definition.class
(constant_definition name: (identifier) @name.definition.constant) @definition.constant
"#;
static GLEAM_TAGS_QUERY: LazyLock<Option<Query>> = LazyLock::new(|| {
    build_tags_query("gleam", &tree_sitter_gleam::LANGUAGE.into(), GLEAM_TAGS_SRC)
});

const DART_TAGS_SRC: &str = r#"
(class_definition name: (identifier) @name.definition.class) @definition.class
(method_signature name: (identifier) @name.definition.function) @definition.function
(function_signature name: (identifier) @name.definition.function) @definition.function
(enum_declaration name: (identifier) @name.definition.class) @definition.class
"#;
static DART_TAGS_QUERY: LazyLock<Option<Query>> =
    LazyLock::new(|| build_tags_query("dart", &tree_sitter_dart::LANGUAGE.into(), DART_TAGS_SRC));

const STARLARK_TAGS_SRC: &str = r#"
(function_statement name: (identifier) @name.definition.function) @definition.function
(assignment left: (identifier) @name.definition.constant) @definition.constant
"#;
static STARLARK_TAGS_QUERY: LazyLock<Option<Query>> = LazyLock::new(|| {
    build_tags_query(
        "starlark",
        &tree_sitter_starlark::LANGUAGE.into(),
        STARLARK_TAGS_SRC,
    )
});

const NIX_TAGS_SRC: &str = r#"
(binding name: (attrpath) @name.definition.constant) @definition.constant
"#;
static NIX_TAGS_QUERY: LazyLock<Option<Query>> =
    LazyLock::new(|| build_tags_query("nix", &tree_sitter_nix::LANGUAGE.into(), NIX_TAGS_SRC));

const ZIG_TAGS_SRC: &str = r#"
(function_declaration name: (identifier) @name.definition.function) @definition.function
(struct_declaration name: (identifier) @name.definition.class) @definition.class
(enum_declaration name: (identifier) @name.definition.class) @definition.class
(const_declaration name: (identifier) @name.definition.constant) @definition.constant
"#;
static ZIG_TAGS_QUERY: LazyLock<Option<Query>> =
    LazyLock::new(|| build_tags_query("zig", &tree_sitter_zig::LANGUAGE.into(), ZIG_TAGS_SRC));

const CSS_TAGS_SRC: &str = r#"
(rule_set (selectors (class_selector (class_name) @name.definition.class))) @definition.class
(rule_set (selectors (id_selector (id_name) @name.definition.class))) @definition.class
"#;
static CSS_TAGS_QUERY: LazyLock<Option<Query>> =
    LazyLock::new(|| build_tags_query("css", &tree_sitter_css::LANGUAGE.into(), CSS_TAGS_SRC));

const FISH_TAGS_SRC: &str = r#"
(function_definition name: (word) @name.definition.function) @definition.function
"#;
static FISH_TAGS_QUERY: LazyLock<Option<Query>> =
    LazyLock::new(|| build_tags_query("fish", &tree_sitter_fish::language(), FISH_TAGS_SRC));

const PERL_TAGS_SRC: &str = r#"
(subroutine_declaration_statement name: (bareword) @name.definition.function) @definition.function
(package_statement (package_name) @name.definition.class) @definition.class
"#;
static PERL_TAGS_QUERY: LazyLock<Option<Query>> =
    LazyLock::new(|| build_tags_query("perl", &tree_sitter_perl::LANGUAGE.into(), PERL_TAGS_SRC));

// SQL DDL: surface the names of schema objects an agent would navigate by.
// DML (select/insert/update/delete) and ALTER/DROP are intentionally not
// matched, so they contribute no tags -- same noise filtering as other
// extractors that ignore usage nodes.
// Note: tree-sitter-sequel as published has no `create_procedure` node, so
// procedures are not captured here either.
const SQL_TAGS_SRC: &str = r#"
(create_table (object_reference) @name.definition.class) @definition.class
(create_view (object_reference) @name.definition.class) @definition.class
(create_materialized_view (object_reference) @name.definition.class) @definition.class
(create_type (object_reference) @name.definition.class) @definition.class
(create_function (object_reference) @name.definition.function) @definition.function
(create_trigger (object_reference) @name.definition.function) @definition.function
(create_index (object_reference) @name.definition.function) @definition.function
(create_schema (identifier) @name.definition.module) @definition.module
"#;
static SQL_TAGS_QUERY: LazyLock<Option<Query>> =
    LazyLock::new(|| build_tags_query("sql", &tree_sitter_sequel::LANGUAGE.into(), SQL_TAGS_SRC));
