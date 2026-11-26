//! What the tests read an index through: a file's index, alone or in a
//! tree of C and C++ headers, each definition's path and the text of its
//! span, and the inputs several modules' tests share. The purity check
//! (`crate::purity`) reads a Rust file's tokens and a manifest's index
//! through it too.

use super::index::{FileKind, Indexed, ScopeId, Symbol};
use super::parse::{BlankMacros, FileMacros, lexed, syntax_of};

pub(crate) use super::index::Scopes;
pub(crate) use super::parse::{RUST_LEXICON, Token, is_comment, parse, text_of, tokens};

/// The index of `bytes`, a file of `kind` alone in its tree: a C or C++
/// file blanks what its own `#define`s make blankable.
pub(crate) fn index(kind: FileKind, bytes: &[u8]) -> Indexed {
    index_in("", kind, bytes, &BlankMacros::default())
}

/// The index of `bytes`, the file at `path` of `kind`, in a tree whose
/// headers' sets are `tree`.
pub(crate) fn index_in(path: &str, kind: FileKind, bytes: &[u8], tree: &BlankMacros) -> Indexed {
    let macros = match kind.blanks_macros() {
        true => tree.of_file(path, &lexed(bytes)),
        false => FileMacros::default(),
    };
    super::resolver::index(kind, bytes, &macros)
}

/// Each definition of `text`, a file of `kind`, as its path joined by
/// `::` and the text of its span, in the index's order.
pub(crate) fn defined(kind: FileKind, text: &str) -> Vec<(String, &str)> {
    let index = index(kind, text.as_bytes()).unwrap();
    let scopes = Scopes(&index.scopes);
    index
        .definitions
        .iter()
        .map(|d| (path(scopes, d.scope), &text[d.span.start..d.span.end]))
        .collect()
}

/// The path of `scope`, its segments joined by `::`.
pub(crate) fn path(scopes: Scopes<'_>, scope: ScopeId) -> String {
    let mut segments: Vec<String> = scopes
        .path_from_end(scope)
        .map(ToString::to_string)
        .collect();
    segments.reverse();
    segments.join("::")
}

/// The text of each definition of `text`, a file of `kind`, that
/// `symbol` names.
pub(crate) fn named<'t>(kind: FileKind, text: &'t str, symbol: &str) -> Vec<&'t str> {
    let symbol = Symbol::parse(symbol, syntax_of(kind)).unwrap();
    let index = index(kind, text.as_bytes()).unwrap();
    let scopes = Scopes(&index.scopes);
    index
        .definitions
        .iter()
        .filter(|d| symbol.names(d.kind, scopes.path_from_end(d.scope)))
        .map(|d| &text[d.span.start..d.span.end])
        .collect()
}

/// A file whose blocks nest `depth` deep, each one space deeper, whose
/// innermost line holds an f-string: the input that, past some 380
/// widths, makes tree-sitter-python's scanner abort the process.
pub(crate) fn nested_ifs(depth: usize) -> String {
    let mut text = String::new();
    for level in 0..depth {
        text.push_str(&format!("{}if x:\n", " ".repeat(level)));
    }
    text.push_str(&format!("{}y = f\"{{z}}\"\n", " ".repeat(depth)));
    text
}
