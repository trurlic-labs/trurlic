//! The sweep that places the general resolver's definitions by span:
//! sorted by start, the widest first, each is under the innermost open
//! container whose span holds it, and two of one span (the names of one
//! statement) are siblings.

use super::{Extent, Found, NameNode};
use crate::code::index::IndexFault;
use crate::code::kinds::{Visibility, What};
use crate::code::query::Container;
use crate::code::scope::{ScopeId, ScopeTree};
use crate::code::symbol::name_segments;

/// A definition placed in its file: its scope, its span and what it is.
pub(super) struct Placed {
    pub(super) scope: ScopeId,
    pub(super) span: Extent,
    pub(super) what: What,
}

/// The visibility the innermost mark that holds `name` states, when
/// `span` holds the mark.
fn visibility_of(name: &NameNode, span: Extent) -> Option<Visibility> {
    name.visibility
        .filter(|mark| span.holds(mark.start, mark.end))
        .map(|mark| mark.visibility)
}

/// A container open in the sweep: its span, kind and scope.
struct Open {
    span: Extent,
    container: Container,
    scope: ScopeId,
}

/// The definitions of `found`, each under the innermost container whose
/// span holds its own, its scope entered in `scopes`. A name with a
/// segment of no text defines nothing. A definition at the file's top
/// that no mark gives a visibility is public when an `@export.name`
/// capture spells its name.
pub(super) fn place(
    found: &Found<'_, '_>,
    scopes: &mut ScopeTree,
) -> Result<Vec<Placed>, IndexFault> {
    let mut open: Vec<Open> = Vec::new();
    let mut placed = Vec::new();
    for (span, name, definer) in found.complete() {
        // Two definitions of one span are siblings.
        while open.last().is_some_and(|top| {
            !top.span.holds(span.start, span.end)
                || (top.span.start, top.span.end) == (span.start, span.end)
        }) {
            open.pop();
        }
        let container = open.last().map_or(Container::Top, |top| top.container);
        if !definer.admits(container) || name.parts.iter().any(|part| part.trim().is_empty()) {
            continue;
        }
        let kind = definer.kind_in(container);
        let parent = open.last().map(|top| top.scope);
        let segments = name.parts.iter().flat_map(|part| name_segments(part));
        let scope = scopes.enter(parent, segments.collect())?;
        if definer.container {
            open.push(Open {
                span,
                container: Container::Kind(kind),
                scope,
            });
        }
        let exported = (container == Container::Top && found.is_exported(&name.parts))
            .then_some(Visibility::Public);
        let what = What {
            kind,
            role: name.role(),
            visibility: visibility_of(name, span)
                .or(exported)
                .or(definer.visibility),
        };
        placed.push(Placed { scope, span, what });
    }
    Ok(placed)
}
