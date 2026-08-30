//! The outline data model: symbol kinds, ranges, extracted symbols, and
//! the tree/directory entries built from them.

use super::lang::LangId;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SymbolKind {
    Function,
    Method,
    Struct,
    Enum,
    Trait,
    TypeAlias,
    Constant,
    Module,
    Impl,
    Macro,
    Class,
    Interface,
    Variable,
    Heading,
    Import,
}

impl SymbolKind {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Function => "fn",
            Self::Method => "me",
            Self::Struct => "st",
            Self::Enum => "en",
            Self::Trait => "tr",
            Self::TypeAlias => "ta",
            Self::Constant => "co",
            Self::Module => "mo",
            Self::Impl => "im",
            Self::Macro => "ma",
            Self::Class => "cl",
            Self::Interface => "if",
            Self::Variable => "va",
            Self::Heading => "hd",
            Self::Import => "im",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Range {
    pub start_row: usize,
    pub start_col: usize,
    pub end_row: usize,
    pub end_col: usize,
}

#[derive(Debug, Clone)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    pub range: Range,
    pub signature: Option<String>,
    pub scope_chain: Vec<String>,
    pub exported: bool,
    pub import_segments: Vec<Vec<String>>,
    pub is_child: bool,
}

#[derive(Debug, Clone)]
pub(super) struct OutlineEntry {
    pub(super) name: String,
    pub(super) kind: SymbolKind,
    pub(super) range: Range,
    pub(super) signature: Option<String>,
    pub(super) exported: bool,
    pub(super) members: Vec<OutlineEntry>,
    pub(super) import_segments: Vec<Vec<String>>,
}

pub(super) struct DirEntry {
    pub(super) rel_path: String,
    #[allow(dead_code)]
    pub(super) name: String,
    pub(super) lang: LangId,
    #[allow(dead_code)]
    pub(super) symbol_count: usize,
    #[allow(dead_code)]
    pub(super) bytes: usize,
    pub(super) tree: Vec<OutlineEntry>,
}
