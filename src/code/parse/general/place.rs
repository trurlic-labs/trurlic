//! The sweep that places the general resolver's definitions by the nodes
//! their patterns captured: sorted by start, the widest first, each is
//! under the innermost open definition or unnamed scope whose node holds
//! its own, and two of one node (the readings of one declarator) are
//! siblings. A span grown over a statement places nothing, so each
//! declarator of `const f = () => {}, a = 1` holds what its own node
//! holds. An export's name is met in the same sweep, in the container
//! that holds it, and makes public the definitions of that name directly
//! in that container wherever they stand in the file.

use std::cmp::Reverse;
use std::collections::BTreeSet;

use super::walk::Found;
use super::{Extent, NameNode, Site, SpanNode};
use crate::code::index::{IndexFault, ScopeId, ScopeTree, Visibility, What, name_segments};
use crate::code::parse::query::{Container, Definer};

/// A definition placed in its file: its scope, its span and what it is.
pub(super) struct Placed {
    pub(super) scope: ScopeId,
    pub(super) span: Extent,
    pub(super) what: What,
}

/// The visibility the source states of the definition named `name` whose
/// span is `span`: the label in force at its name, else the innermost
/// mark that holds its name when the span holds the mark, else private
/// when the source marks a place private from the span's start to its
/// name.
fn stated(found: &Found<'_, '_>, name: &NameNode, span: Extent) -> Option<Visibility> {
    let marked = found.is_marked_private(span.start, name.start);
    name.label
        .or(name
            .visibility
            .filter(|mark| span.holds(mark.start, mark.end))
            .map(|mark| mark.visibility))
        .or(marked.then_some(Visibility::Private))
}

/// What the sweep meets: a definition, a node captured as an unnamed
/// scope, or one an `@export.name` capture spells.
enum Met<'f, 'r, 't> {
    Definition(Site, &'f NameNode, &'r Definer),
    Unnamed(SpanNode),
    Export(SpanNode, &'t str),
}

impl Met<'_, '_, '_> {
    const fn node(&self) -> SpanNode {
        match self {
            Self::Definition(site, _, _) => site.node,
            Self::Unnamed(node) | Self::Export(node, _) => *node,
        }
    }
}

/// A definition no mark states the visibility of, which an export of its
/// name in its container makes public: its place among the placed, its
/// container's scope and its name.
struct Unstated<'f> {
    at: usize,
    parent: Option<ScopeId>,
    name: &'f str,
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
fn met<'f, 'r, 't>(found: &'f Found<'r, 't>) -> Vec<Met<'f, 'r, 't>> {
    let unnamed = found
        .unnamed
        .iter()
        .filter(|node| !found.no_scope.contains(&node.id))
        .map(|node| Met::Unnamed(*node));
    let exported = found
        .exported
        .iter()
        .map(|(node, name)| Met::Export(*node, name));
    let mut met: Vec<Met<'_, '_, '_>> = found
        .complete()
        .map(|(site, name, definer)| Met::Definition(site, name, definer))
        .chain(unnamed)
        .chain(exported)
        .collect();
    met.sort_by_key(|met| {
        let node = met.node();
        (
            node.start,
            Reverse(node.end),
            !matches!(met, Met::Definition(..)),
        )
    });
    met
}

/// The definitions of `found`, each under the innermost open definition
/// or unnamed scope whose node holds its own, its scope entered in
/// `scopes`. A name with a segment of no text defines nothing. A
/// definition that no mark gives a visibility is public when an
/// `@export.name` capture in its container spells its name, else of the
/// innermost `@visibility.within` mark that holds its name, else of its
/// pattern.
pub(super) fn place(
    found: &Found<'_, '_>,
    scopes: &mut ScopeTree,
) -> Result<Vec<Placed>, IndexFault> {
    let mut sweep = Sweep::default();
    for met in met(found) {
        match met {
            Met::Definition(site, name, definer) => {
                sweep.define(site, (name, definer), (found, scopes))?
            }
            Met::Unnamed(node) => enter_unnamed(&mut sweep.open, node),
            Met::Export(node, name) => {
                close_until_holding(&mut sweep.open, node);
                let parent = parent_of(&sweep.open);
                sweep.exports.extend(parent.map(|parent| (parent, name)));
            }
        }
    }
    let Sweep {
        mut placed,
        unstated,
        exports,
        ..
    } = sweep;
    for exported in unstated
        .iter()
        .filter(|it| exports.contains(&(it.parent, it.name)))
    {
        if let Some(definition) = placed.get_mut(exported.at) {
            definition.what.visibility = Some(Visibility::Public);
        }
    }
    Ok(placed)
}

/// The sweep's state: the open definitions and unnamed scopes, the
/// definitions placed, those of them no mark states the visibility of,
/// and the names an export spells, by the scope its container is.
#[derive(Default)]
struct Sweep<'f> {
    open: Vec<Open>,
    placed: Vec<Placed>,
    unstated: Vec<Unstated<'f>>,
    exports: BTreeSet<(Option<ScopeId>, &'f str)>,
}

impl<'f> Sweep<'f> {
    /// Place the definition `definer` makes at `site`, named `name`,
    /// under the innermost open entry that holds it, unless it defines
    /// nothing there.
    fn define(
        &mut self,
        site: Site,
        (name, definer): (&'f NameNode, &Definer),
        (found, scopes): (&Found<'_, '_>, &mut ScopeTree),
    ) -> Result<(), IndexFault> {
        close_until_holding(&mut self.open, site.node);
        // In an unnamed scope no definition holds, nothing is defined.
        let Some(parent) = parent_of(&self.open) else {
            return Ok(());
        };
        let container = self.open.last().map_or(Container::Top, |top| top.container);
        if !definer.admits(container) || name.parts.iter().any(|part| part.trim().is_empty()) {
            return Ok(());
        }
        let kind = definer.kind_in(container);
        let segments = name.parts.iter().flat_map(|part| name_segments(part));
        let scope = scopes.enter(parent, segments.collect())?;
        if definer.container {
            self.open.push(Open {
                node: site.node,
                container: Container::Kind(kind),
                parent: Some(scope),
            });
        }
        let visibility = stated(found, name, site.span);
        if let (None, [one]) = (visibility, name.parts.as_slice()) {
            let at = self.placed.len();
            self.unstated.push(Unstated {
                at,
                parent,
                name: one,
            });
        }
        let what = What {
            kind,
            role: name.role(),
            visibility: visibility.or(name.within).or(definer.visibility),
        };
        self.placed.push(Placed {
            scope,
            span: site.span,
            what,
        });
        Ok(())
    }
}

/// The scope a definition directly inside the innermost of `open` is
/// under: `None` within the file's top, nothing in an unnamed scope that
/// no definition holds.
fn parent_of(open: &[Open]) -> Option<Option<ScopeId>> {
    open.last().map_or(Some(None), |top| top.parent.map(Some))
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
