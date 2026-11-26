//! What the resolvers' tests read an index through: a file's index, alone
//! or in a tree of C and C++ headers, and each definition's path and the
//! text of its span.

use super::encoding::Indexed;
use super::index::FileKind;
use super::language::c_family::{BlankMacros, FileMacros, lexed};
use super::scope::{ScopeId, Scopes};
use super::symbol::Symbol;

/// The index of `bytes`, a file of `kind` alone in its tree: a C or C++
/// file blanks what its own `#define`s make blankable.
pub(crate) fn index(kind: FileKind, bytes: &[u8]) -> Indexed {
    index_in(kind, bytes, &BlankMacros::default())
}

/// The index of `bytes`, a file of `kind` in a tree whose headers' set is
/// `tree`.
pub(crate) fn index_in(kind: FileKind, bytes: &[u8], tree: &BlankMacros) -> Indexed {
    let macros = match kind.blanks_macros() {
        true => tree.of_file(&lexed(bytes)),
        false => FileMacros::default(),
    };
    super::index::index(kind, bytes, &macros)
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
    let symbol = Symbol::parse(symbol, kind.syntax()).unwrap();
    let index = index(kind, text.as_bytes()).unwrap();
    let scopes = Scopes(&index.scopes);
    index
        .definitions
        .iter()
        .filter(|d| symbol.names(d.kind, scopes.path_from_end(d.scope)))
        .map(|d| &text[d.span.start..d.span.end])
        .collect()
}
