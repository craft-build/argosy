//! The per-language tree-sitter query sources and their compiled
//! `LazyLock` caches, plus the lookup helpers over them.

use std::sync::LazyLock;

use tracing::error;
use tree_sitter::{Language, Query};

use super::lang::LangId;

pub(super) fn lang_parts(lang: LangId) -> (&'static LazyLock<Option<Query>>, &'static str) {
    match lang {
        LangId::Rust => (&RUST_QUERY, RUST_SRC),
        LangId::TypeScript => (&TS_QUERY, TS_SRC),
        LangId::Tsx => (&TSX_QUERY, TS_SRC),
        LangId::Python => (&PY_QUERY, PY_SRC),
        LangId::Go => (&GO_QUERY, GO_SRC),
        LangId::Java => (&JAVA_QUERY, JAVA_SRC),
        LangId::C => (&C_QUERY, C_SRC),
        LangId::Cpp => (&CPP_QUERY, CPP_SRC),
        LangId::Ruby => (&RUBY_QUERY, RUBY_SRC),
        LangId::Lua => (&LUA_QUERY, LUA_SRC),
        LangId::Bash => (&BASH_QUERY, BASH_SRC),
        LangId::Kotlin => (&KT_QUERY, KT_SRC),
        LangId::Swift => (&SWIFT_QUERY, SWIFT_SRC),
        LangId::CSharp => (&CSHARP_QUERY, CSHARP_SRC),
        LangId::Elixir => (&ELIXIR_QUERY, ELIXIR_SRC),
        LangId::Scala => (&SCALA_QUERY, SCALA_SRC),
        LangId::Php => (&PHP_QUERY, PHP_SRC),
        LangId::Html => (&HTML_QUERY, HTML_SRC),
        LangId::Gleam => (&GLEAM_QUERY, GLEAM_SRC),
        LangId::Dart => (&DART_QUERY, DART_SRC),
        LangId::Starlark => (&STARLARK_QUERY, STARLARK_SRC),
        LangId::Nix => (&NIX_QUERY, NIX_SRC),
        LangId::Zig => (&ZIG_QUERY, ZIG_SRC),
        LangId::Markdown => (&MD_QUERY, MD_SRC),
        LangId::Css => (&CSS_QUERY, CSS_SRC),
        LangId::Fish => (&FISH_QUERY, FISH_SRC),
        LangId::Gdscript => (&GDSCRIPT_QUERY, GDSCRIPT_SRC),
        LangId::Gdshader => (&GDSHADER_QUERY, GDSHADER_SRC),
        LangId::GodotResource => (&GODOT_RESOURCE_QUERY, GODOT_RESOURCE_SRC),
        LangId::ObjC => (&OBJC_QUERY, OBJC_SRC),
        LangId::Perl => (&PERL_QUERY, PERL_SRC),
        LangId::SvelteNext => (&SVELTE_NEXT_QUERY, SVELTE_NEXT_SRC),
        LangId::Zsh => (&ZSH_QUERY, ZSH_SRC),
        LangId::Sql => (&SQL_QUERY, SQL_SRC),
        LangId::Yaml => (&YAML_QUERY, YAML_SRC),
        LangId::Toml => (&TOML_QUERY, TOML_SRC),
    }
}

pub(super) fn lang_query(lang: LangId) -> Option<&'static Query> {
    lang_parts(lang).0.as_ref()
}

#[cfg(test)]
pub(super) fn query_source(lang: LangId) -> &'static str {
    lang_parts(lang).1
}

fn build_query(lang: &'static str, language: &Language, src: &'static str) -> Option<Query> {
    match Query::new(language, src) {
        Ok(q) => Some(q),
        Err(e) => {
            error!(error = %e, lang, "outline query failed to compile, language will be skipped");
            None
        }
    }
}

#[cfg(test)]
pub(super) const ALL_LANGS: &[LangId] = &[
    LangId::Rust,
    LangId::TypeScript,
    LangId::Python,
    LangId::Go,
    LangId::Java,
    LangId::C,
    LangId::Cpp,
    LangId::Ruby,
    LangId::Lua,
    LangId::Bash,
    LangId::Kotlin,
    LangId::Swift,
    LangId::CSharp,
    LangId::Elixir,
    LangId::Scala,
    LangId::Php,
    LangId::Html,
    LangId::Gleam,
    LangId::Dart,
    LangId::Starlark,
    LangId::Nix,
    LangId::Zig,
    LangId::Markdown,
    LangId::Css,
    LangId::Fish,
    LangId::Gdscript,
    LangId::Gdshader,
    LangId::GodotResource,
    LangId::ObjC,
    LangId::Perl,
    LangId::SvelteNext,
    LangId::Zsh,
    LangId::Sql,
    LangId::Yaml,
];

const RUST_SRC: &str = r#"
(function_item name: (identifier) @fn.name) @fn.def
(impl_item type: (type_identifier) @impl.name) @impl.def
(struct_item name: (type_identifier) @struct.name) @struct.def
(enum_item name: (type_identifier) @enum.name) @enum.def
(trait_item name: (type_identifier) @trait.name) @trait.def
(type_item name: (type_identifier) @type.name) @type.def
(const_item name: (identifier) @const.name) @const.def
(mod_item name: (identifier) @mod.name) @mod.def
(macro_definition name: (identifier) @macro.name) @macro.def
(use_declaration) @import.def
(field_declaration_list (field_declaration name: (field_identifier) @field.name)) @field.def
(enum_variant_list (enum_variant name: (identifier) @variant.name)) @variant.def
"#;
static RUST_QUERY: LazyLock<Option<Query>> =
    LazyLock::new(|| build_query("rust", &tree_sitter_rust::LANGUAGE.into(), RUST_SRC));

const TS_SRC: &str = r#"
(function_declaration name: (identifier) @fn.name) @fn.def
(method_definition name: (property_identifier) @method.name) @method.def
(class_declaration name: (type_identifier) @class.name) @class.def
(interface_declaration name: (type_identifier) @iface.name) @iface.def
(type_alias_declaration name: (type_identifier) @type.name) @type.def
(variable_declarator name: (identifier) @var.name) @var.def
(import_statement) @import.def
(class_body (public_field_definition name: (property_identifier) @field.name)) @field.def
"#;
static TS_QUERY: LazyLock<Option<Query>> = LazyLock::new(|| {
    build_query(
        "typescript",
        &tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        TS_SRC,
    )
});

static TSX_QUERY: LazyLock<Option<Query>> =
    LazyLock::new(|| build_query("tsx", &tree_sitter_typescript::LANGUAGE_TSX.into(), TS_SRC));

const PY_SRC: &str = r#"
(function_definition name: (identifier) @fn.name) @fn.def
(class_definition name: (identifier) @class.name) @class.def
(import_statement) @import.def
(import_from_statement) @import.def
"#;
static PY_QUERY: LazyLock<Option<Query>> =
    LazyLock::new(|| build_query("python", &tree_sitter_python::LANGUAGE.into(), PY_SRC));

const GO_SRC: &str = r#"
(function_declaration name: (identifier) @fn.name) @fn.def
(method_declaration name: (field_identifier) @method.name) @method.def
(type_declaration (type_spec name: (type_identifier) @type.name)) @type.def
(type_declaration (type_alias name: (type_identifier) @type.name)) @type.def
(import_declaration) @import.def
"#;
static GO_QUERY: LazyLock<Option<Query>> =
    LazyLock::new(|| build_query("go", &tree_sitter_go::LANGUAGE.into(), GO_SRC));

const JAVA_SRC: &str = r#"
(class_declaration name: (identifier) @class.name) @class.def
(method_declaration name: (identifier) @method.name) @method.def
(interface_declaration name: (identifier) @iface.name) @iface.def
(import_declaration) @import.def
"#;
static JAVA_QUERY: LazyLock<Option<Query>> =
    LazyLock::new(|| build_query("java", &tree_sitter_java::LANGUAGE.into(), JAVA_SRC));

const C_SRC: &str = r#"
(function_definition declarator: (function_declarator declarator: (identifier) @fn.name)) @fn.def
"#;
static C_QUERY: LazyLock<Option<Query>> =
    LazyLock::new(|| build_query("c", &tree_sitter_c::LANGUAGE.into(), C_SRC));

const CPP_SRC: &str = r#"
(function_definition declarator: (function_declarator declarator: (identifier) @fn.name)) @fn.def
(class_specifier name: (type_identifier) @class.name) @class.def
"#;
static CPP_QUERY: LazyLock<Option<Query>> =
    LazyLock::new(|| build_query("cpp", &tree_sitter_cpp::LANGUAGE.into(), CPP_SRC));

const RUBY_SRC: &str = r#"
(method name: (identifier) @method.name) @method.def
(class name: (constant) @class.name) @class.def
(module name: (constant) @mod.name) @mod.def
(call method: (identifier) @_require arguments: (argument_list (string) @import.name) (#eq? @_require "require")) @import.def
"#;
static RUBY_QUERY: LazyLock<Option<Query>> =
    LazyLock::new(|| build_query("ruby", &tree_sitter_ruby::LANGUAGE.into(), RUBY_SRC));

const LUA_SRC: &str = r#"
(function_declaration name: (identifier) @fn.name) @fn.def
"#;
static LUA_QUERY: LazyLock<Option<Query>> =
    LazyLock::new(|| build_query("lua", &tree_sitter_lua::LANGUAGE.into(), LUA_SRC));

const BASH_SRC: &str = r#"
(function_definition name: (word) @fn.name) @fn.def
"#;
static BASH_QUERY: LazyLock<Option<Query>> =
    LazyLock::new(|| build_query("bash", &tree_sitter_bash::LANGUAGE.into(), BASH_SRC));

const KT_SRC: &str = r#"
(function_declaration name: (identifier) @fn.name) @fn.def
(class_declaration name: (identifier) @class.name) @class.def
"#;
static KT_QUERY: LazyLock<Option<Query>> =
    LazyLock::new(|| build_query("kotlin", &tree_sitter_kotlin_ng::LANGUAGE.into(), KT_SRC));

const SWIFT_SRC: &str = r#"
(function_declaration name: (identifier) @fn.name) @fn.def
"#;
static SWIFT_QUERY: LazyLock<Option<Query>> =
    LazyLock::new(|| build_query("swift", &tree_sitter_swift::LANGUAGE.into(), SWIFT_SRC));

const CSHARP_SRC: &str = r#"
(class_declaration name: (identifier) @class.name) @class.def
(method_declaration name: (identifier) @method.name) @method.def
(struct_declaration name: (identifier) @struct.name) @struct.def
(interface_declaration name: (identifier) @iface.name) @iface.def
(enum_declaration name: (identifier) @enum.name) @enum.def
(using_directive) @import.def
"#;
static CSHARP_QUERY: LazyLock<Option<Query>> =
    LazyLock::new(|| build_query("csharp", &tree_sitter_c_sharp::LANGUAGE.into(), CSHARP_SRC));

const ELIXIR_SRC: &str = r#"
(call target: (identifier) @_def (arguments (alias) @fn.name) (#eq? @_def "def")) @fn.def
"#;
static ELIXIR_QUERY: LazyLock<Option<Query>> =
    LazyLock::new(|| build_query("elixir", &tree_sitter_elixir::LANGUAGE.into(), ELIXIR_SRC));

const SCALA_SRC: &str = r#"
(function_definition name: (identifier) @fn.name) @fn.def
(class_definition name: (identifier) @class.name) @class.def
(object_definition name: (identifier) @mod.name) @mod.def
"#;
static SCALA_QUERY: LazyLock<Option<Query>> =
    LazyLock::new(|| build_query("scala", &tree_sitter_scala::LANGUAGE.into(), SCALA_SRC));

const PHP_SRC: &str = r#"
(function_definition name: (name) @fn.name) @fn.def
(class_declaration name: (name) @class.name) @class.def
(method_declaration name: (name) @method.name) @method.def
(use_declaration) @import.def
"#;
static PHP_QUERY: LazyLock<Option<Query>> =
    LazyLock::new(|| build_query("php", &tree_sitter_php::LANGUAGE_PHP.into(), PHP_SRC));

const HTML_SRC: &str = r#"
(element (start_tag (tag_name) @heading.name)) @heading.def
"#;
static HTML_QUERY: LazyLock<Option<Query>> =
    LazyLock::new(|| build_query("html", &tree_sitter_html::LANGUAGE.into(), HTML_SRC));

const GLEAM_SRC: &str = r#"
(function name: (identifier) @fn.name) @fn.def
(constant name: (identifier) @const.name) @const.def
(type name: (type_identifier) @type.name) @type.def
(import) @import.def
"#;
static GLEAM_QUERY: LazyLock<Option<Query>> =
    LazyLock::new(|| build_query("gleam", &tree_sitter_gleam::LANGUAGE.into(), GLEAM_SRC));

const DART_SRC: &str = r#"
(function_signature name: (identifier) @fn.name) @fn.def
(class_declaration name: (identifier) @class.name) @class.def
(enum_declaration name: (identifier) @enum.name) @enum.def
(import_specification) @import.def
"#;
static DART_QUERY: LazyLock<Option<Query>> =
    LazyLock::new(|| build_query("dart", &tree_sitter_dart::LANGUAGE.into(), DART_SRC));

const STARLARK_SRC: &str = r#"
(function_definition name: (identifier) @fn.name) @fn.def
"#;
static STARLARK_QUERY: LazyLock<Option<Query>> = LazyLock::new(|| {
    build_query(
        "starlark",
        &tree_sitter_starlark::LANGUAGE.into(),
        STARLARK_SRC,
    )
});

const NIX_SRC: &str = r#"
(binding attrpath: (attrpath) @var.name) @var.def
"#;
static NIX_QUERY: LazyLock<Option<Query>> =
    LazyLock::new(|| build_query("nix", &tree_sitter_nix::LANGUAGE.into(), NIX_SRC));

const ZIG_SRC: &str = r#"
(function_declaration name: (identifier) @fn.name) @fn.def
"#;
static ZIG_QUERY: LazyLock<Option<Query>> =
    LazyLock::new(|| build_query("zig", &tree_sitter_zig::LANGUAGE.into(), ZIG_SRC));

const MD_SRC: &str = r#"
(atx_heading (atx_h1_marker) (inline) @heading.name) @heading.def
(atx_heading (atx_h2_marker) (inline) @heading.name) @heading.def
(atx_heading (atx_h3_marker) (inline) @heading.name) @heading.def
"#;
static MD_QUERY: LazyLock<Option<Query>> =
    LazyLock::new(|| build_query("markdown", &tree_sitter_md::LANGUAGE.into(), MD_SRC));

const CSS_SRC: &str = r#"
(rule_set (selectors) @class.name) @class.def
"#;
static CSS_QUERY: LazyLock<Option<Query>> =
    LazyLock::new(|| build_query("css", &tree_sitter_css::LANGUAGE.into(), CSS_SRC));

const FISH_SRC: &str = r#"
(function_definition name: (word) @fn.name) @fn.def
"#;
static FISH_QUERY: LazyLock<Option<Query>> =
    LazyLock::new(|| build_query("fish", &tree_sitter_fish::language(), FISH_SRC));

const GDSCRIPT_SRC: &str = r#"
(class_definition name: (name) @class.name) @class.def
(function_definition name: (name) @fn.name) @fn.def
"#;
static GDSCRIPT_QUERY: LazyLock<Option<Query>> = LazyLock::new(|| {
    build_query(
        "gdscript",
        &tree_sitter_gdscript::LANGUAGE.into(),
        GDSCRIPT_SRC,
    )
});

const GDSHADER_SRC: &str = r#"
(function_definition declarator: (identifier) @fn.name) @fn.def
(struct_definition name: (identifier) @struct.name) @struct.def
"#;
static GDSHADER_QUERY: LazyLock<Option<Query>> = LazyLock::new(|| {
    build_query(
        "gdshader",
        &tree_sitter_gdshader::LANGUAGE.into(),
        GDSHADER_SRC,
    )
});

const GODOT_RESOURCE_SRC: &str = r#"
(section (identifier) @class.name) @class.def
"#;
static GODOT_RESOURCE_QUERY: LazyLock<Option<Query>> = LazyLock::new(|| {
    build_query(
        "godot-resource",
        &tree_sitter_godot_resource::LANGUAGE.into(),
        GODOT_RESOURCE_SRC,
    )
});

const OBJC_SRC: &str = r#"
(class_interface (identifier) @class.name) @class.def
(class_implementation (identifier) @class.name) @class.def
(protocol_declaration (identifier) @iface.name) @iface.def
(method_declaration) @method.def
(function_definition) @fn.def
"#;
static OBJC_QUERY: LazyLock<Option<Query>> =
    LazyLock::new(|| build_query("objc", &tree_sitter_objc::LANGUAGE.into(), OBJC_SRC));

const PERL_SRC: &str = r#"
(package_statement (package_name) @mod.name) @mod.def
(require_statement package_name: (package_name) @import.name) @import.def
"#;
static PERL_QUERY: LazyLock<Option<Query>> =
    LazyLock::new(|| build_query("perl", &tree_sitter_perl::LANGUAGE.into(), PERL_SRC));

const SVELTE_NEXT_SRC: &str = r#"
(element (start_tag (tag_name) @heading.name)) @heading.def
"#;
static SVELTE_NEXT_QUERY: LazyLock<Option<Query>> = LazyLock::new(|| {
    build_query(
        "svelte-next",
        &tree_sitter_svelte_next::LANGUAGE.into(),
        SVELTE_NEXT_SRC,
    )
});

const ZSH_SRC: &str = r#"
(function_definition name: (word) @fn.name) @fn.def
"#;
static ZSH_QUERY: LazyLock<Option<Query>> =
    LazyLock::new(|| build_query("zsh", &tree_sitter_zsh::LANGUAGE.into(), ZSH_SRC));

const YAML_SRC: &str = r#"
(block_mapping_pair key: (_) @const.name) @const.def
(flow_pair key: (_) @const.name) @const.def
"#;
static YAML_QUERY: LazyLock<Option<Query>> =
    LazyLock::new(|| build_query("yaml", &tree_sitter_yaml::LANGUAGE.into(), YAML_SRC));

// TOML has no functions, classes, or imports, just nested tables of key/value
// pairs, so `extract_toml_symbols` walks the tree directly instead of running
// this query. The query is kept only so `all_queries_compile_against_grammar`
// stays uniform across every language; the grammar declares no named fields on
// `pair`, so the query matches the node itself rather than a keyed child.
const TOML_SRC: &str = "(pair) @const.def\n";
static TOML_QUERY: LazyLock<Option<Query>> =
    LazyLock::new(|| build_query("toml", &tree_sitter_toml_ng::LANGUAGE.into(), TOML_SRC));

// SQL DDL: surface the shape of schema objects (tables, views, materialized
// views, types, functions, triggers, indexes, schemas). DML (SELECT/INSERT/
// UPDATE/DELETE) and ALTER/DROP are not matched, so they contribute no
// symbols -- same noise filtering as other extractors that ignore usage nodes.
// The `.` immediate-sibling anchor pins the name to the token right after the
// leading keyword (trigger/index/schema names share their node type with later
// references inside the same statement). tree-sitter-sequel as published has no
// `create_procedure` node, so procedures are not captured here either.
const SQL_SRC: &str = r#"
(create_table (object_reference) @class.name) @class.def
(create_view (object_reference) @class.name) @class.def
(create_materialized_view (object_reference) @class.name) @class.def
(create_type (object_reference) @class.name) @class.def
(create_function (object_reference) @fn.name) @fn.def
(create_trigger (keyword_trigger) . (object_reference) @fn.name) @fn.def
(create_index (keyword_index) . (identifier) @fn.name) @fn.def
(create_schema (keyword_schema) . (identifier) @mod.name) @mod.def
(create_table (column_definitions (column_definition name: (identifier) @field.name) @field.def))
(create_type (column_definitions (column_definition name: (identifier) @field.name) @field.def))
"#;
static SQL_QUERY: LazyLock<Option<Query>> =
    LazyLock::new(|| build_query("sql", &tree_sitter_sequel::LANGUAGE.into(), SQL_SRC));
