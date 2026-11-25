//! What the resolvers' tests read an index through: each definition's
//! path and the text of its span.

use super::index::{FileKind, index};
use super::scope::{ScopeId, Scopes};
use super::symbol::Symbol;

/// Each definition of `text`, a file of `kind`, as its path joined by
/// `::` and the text of its span, in the index's order.
pub(crate) fn defined(kind: FileKind, text: &str) -> Vec<(String, &str)> {
    let index = index(kind, text.as_bytes()).unwrap();
    let scopes = Scopes {
        file: &index.scopes,
        added: &[],
    };
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
    let scopes = Scopes {
        file: &index.scopes,
        added: &[],
    };
    index
        .definitions
        .iter()
        .filter(|d| symbol.names(d.kind, scopes.path_from_end(d.scope)))
        .map(|d| &text[d.span.start..d.span.end])
        .collect()
}
