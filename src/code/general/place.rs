//! The sweep that places the general resolver's definitions by the nodes
//! their patterns captured: sorted by start, the widest first, each is
//! under the innermost open definition or unnamed scope whose node holds
//! its own, and two of one node (the readings of one declarator) are
//! siblings. A span grown over a statement places nothing, so each
//! declarator of `const f = () => {}, a = 1` holds what its own node
//! holds.

use std::cmp::Reverse;

use super::{Extent, Found, NameNode, Site, SpanNode};
use crate::code::index::IndexFault;
use crate::code::kinds::{Visibility, What};
use crate::code::query::{Container, Definer};
use crate::code::scope::{ScopeId, ScopeTree};
use crate::code::symbol::name_segments;

/// A definition placed in its file: its scope, its span and what it is.
pub(super) struct Placed {
    pub(super) scope: ScopeId,
    pub(super) span: Extent,
    pub(super) what: What,
}

/// The visibility the source states of the definition named `name` whose
/// span is `span`, in `container`: the label in force at its name, else
/// the innermost mark that holds its name when the span holds the mark,
/// else the innermost `@visibility.within` mark's, else public at the
/// file's top when an `@export.name` capture spells its name.
fn visibility_of(
    found: &Found<'_, '_>,
    container: Container,
    name: &NameNode,
    span: Extent,
) -> Option<Visibility> {
    let exported = container == Container::Top && found.is_exported(&name.parts);
    name.label
        .or(name
            .visibility
            .filter(|mark| span.holds(mark.start, mark.end))
            .map(|mark| mark.visibility))
        .or(name.within)
        .or(exported.then_some(Visibility::Public))
}

/// What the sweep meets: a definition, or a node captured as an unnamed
/// scope.
enum Met<'f, 'r> {
    Definition(Site, &'f NameNode, &'r Definer),
    Unnamed(SpanNode),
}

impl Met<'_, '_> {
    const fn node(&self) -> SpanNode {
        match self {
            Self::Definition(site, _, _) => site.node,
            Self::Unnamed(node) => *node,
        }
    }
}

/// An open definition or unnamed scope: its node, the container a
/// definition directly inside it is in, and the scope that definition's
/// path is under; `None` for an unnamed scope no definition holds, inside
/// which nothing is defined.
struct Open {
    node: SpanNode,
    container: Container,
    parent: Option<ScopeId>,
}

/// What the sweep meets in `found`, by node start, the widest first, a
/// definition before an unnamed scope of its own node. A node captured as
/// no scope is none.
fn met<'f, 'r>(found: &'f Found<'r, '_>) -> Vec<Met<'f, 'r>> {
    let unnamed = found
        .unnamed
        .iter()
        .filter(|node| !found.no_scope.contains(&node.id))
        .map(|node| Met::Unnamed(*node));
    let mut met: Vec<Met<'_, '_>> = found
        .complete()
        .map(|(site, name, definer)| Met::Definition(site, name, definer))
        .chain(unnamed)
        .collect();
    met.sort_by_key(|met| {
        let node = met.node();
        (
            node.start,
            Reverse(node.end),
            matches!(met, Met::Unnamed(_)),
        )
    });
    met
}

/// The definitions of `found`, each under the innermost open definition
/// or unnamed scope whose node holds its own, its scope entered in
/// `scopes`. A name with a segment of no text defines nothing. A
/// definition at the file's top that no mark gives a visibility is public
/// when an `@export.name` capture spells its name.
pub(super) fn place(
    found: &Found<'_, '_>,
    scopes: &mut ScopeTree,
) -> Result<Vec<Placed>, IndexFault> {
    let mut open: Vec<Open> = Vec::new();
    let mut placed = Vec::new();
    for met in met(found) {
        let node = met.node();
        let Met::Definition(site, name, definer) = met else {
            enter_unnamed(&mut open, node);
            continue;
        };
        close_until_holding(&mut open, node);
        // In an unnamed scope no definition holds, nothing is defined.
        let Some(parent) = open.last().map_or(Some(None), |top| top.parent.map(Some)) else {
            continue;
        };
        let container = open.last().map_or(Container::Top, |top| top.container);
        if !definer.admits(container) || name.parts.iter().any(|part| part.trim().is_empty()) {
            continue;
        }
        let kind = definer.kind_in(container);
        let segments = name.parts.iter().flat_map(|part| name_segments(part));
        let scope = scopes.enter(parent, segments.collect())?;
        if definer.container {
            open.push(Open {
                node,
                container: Container::Kind(kind),
                parent: Some(scope),
            });
        }
        let what = What {
            kind,
            role: name.role(),
            visibility: visibility_of(found, container, name, site.span).or(definer.visibility),
        };
        placed.push(Placed {
            scope,
            span: site.span,
            what,
        });
    }
    Ok(placed)
}

/// Close each open entry that does not hold `node`, and one of `node`
/// itself: two definitions of one node are siblings.
fn close_until_holding(open: &mut Vec<Open>, node: SpanNode) {
    while open
        .last()
        .is_some_and(|top| !top.node.holds(node) || top.node.id == node.id)
    {
        open.pop();
    }
}

/// Open the unnamed scope `node`, unless the definition open at its node
/// names it. What it defines goes under the definition around it; at the
/// file's top, or in an unnamed scope that has none, there is none.
fn enter_unnamed(open: &mut Vec<Open>, node: SpanNode) {
    if open.last().is_some_and(|top| top.node.id == node.id) {
        return;
    }
    close_until_holding(open, node);
    let parent = open.last().and_then(|top| top.parent);
    open.push(Open {
        node,
        container: Container::Unnamed,
        parent,
    });
}
