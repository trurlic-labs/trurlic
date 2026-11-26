//! What a file of each kind is read as: the language row of a kind the
//! general resolver reads, the spelling of its symbols, its grammar's
//! identity and its name in a cache key. Each is a function of the kind
//! here, beside the rows it reads, so a caller's edge to the language
//! rows is a path it names.

use std::sync::LazyLock;

use super::{C, CPP, Language, PYTHON, TSX, TYPESCRIPT, TYPESCRIPT_DECLARATIONS};
use crate::code::index::{FileKind, Syntax};
use crate::code::parse::parser::grammar_identity;

/// The identity of tree-sitter-rust, computed once.
static RUST_GRAMMAR: LazyLock<[u8; 32]> = LazyLock::new(|| {
    grammar_identity(
        &tree_sitter_rust::LANGUAGE.into(),
        tree_sitter_rust::NODE_TYPES,
    )
});

/// The language the general resolver reads a file of `kind` as; `None`
/// for Rust and the lexical kinds.
pub(in crate::code) const fn language_of(kind: FileKind) -> Option<&'static Language> {
    match kind {
        FileKind::Python => Some(&PYTHON),
        FileKind::TypeScript => Some(&TYPESCRIPT),
        FileKind::TypeScriptDeclarations => Some(&TYPESCRIPT_DECLARATIONS),
        FileKind::Tsx => Some(&TSX),
        FileKind::C => Some(&C),
        FileKind::Cpp => Some(&CPP),
        FileKind::Rust | FileKind::Toml | FileKind::Json | FileKind::Make | FileKind::Markdown => {
            None
        }
    }
}

/// How a symbol on a file of `kind` is spelled: a language's by its row.
pub(in crate::code) const fn syntax_of(kind: FileKind) -> Syntax {
    match kind {
        FileKind::Rust => Syntax::Rust,
        FileKind::Python => PYTHON.syntax,
        FileKind::TypeScript => TYPESCRIPT.syntax,
        FileKind::TypeScriptDeclarations => TYPESCRIPT_DECLARATIONS.syntax,
        FileKind::Tsx => TSX.syntax,
        FileKind::C => C.syntax,
        FileKind::Cpp => CPP.syntax,
        FileKind::Toml | FileKind::Json | FileKind::Make | FileKind::Markdown => Syntax::Plain,
    }
}

/// The identity of the grammar a file of `kind` is parsed with, in a
/// cache key: a grammar's new version may parse the same bytes into
/// another tree, which no fixture of the index need show. A lexical
/// kind's rules are the index's own, so its identity is
/// [`INDEX_FORMAT`](crate::code::INDEX_FORMAT)'s. A language the general
/// resolver reads adds its query file, whose patterns are rules of its
/// index.
pub(crate) fn grammar_identity_of(kind: FileKind) -> [u8; 32] {
    match kind {
        FileKind::Rust => *RUST_GRAMMAR,
        FileKind::Python => PYTHON.identity(),
        FileKind::TypeScript => TYPESCRIPT.identity(),
        FileKind::TypeScriptDeclarations => TYPESCRIPT_DECLARATIONS.identity(),
        FileKind::Tsx => TSX.identity(),
        FileKind::C => C.identity(),
        FileKind::Cpp => CPP.identity(),
        FileKind::Toml | FileKind::Json | FileKind::Make | FileKind::Markdown => [0; 32],
    }
}

/// The name of `kind` in a cache key.
pub(crate) const fn name_of(kind: FileKind) -> &'static str {
    match kind {
        FileKind::Rust => "rust",
        FileKind::Python => PYTHON.name,
        FileKind::TypeScript => TYPESCRIPT.name,
        FileKind::TypeScriptDeclarations => TYPESCRIPT_DECLARATIONS.name,
        FileKind::Tsx => TSX.name,
        FileKind::C => C.name,
        FileKind::Cpp => CPP.name,
        FileKind::Toml => "toml",
        FileKind::Json => "json",
        FileKind::Make => "make",
        FileKind::Markdown => "markdown",
    }
}
