//! The leaves of a `pub use`: each name it brings in is a re-export of
//! the module it stands in, unless the file defines that path itself
//! (`pub(crate) use name;` after `macro_rules! name`), which only the end
//! of the walk knows.

use std::collections::BTreeSet;
use std::ops::Range;

use tree_sitter::Node;

use crate::code::index::IndexFault;
use crate::code::kinds::{DefinitionKind, Visibility};
use crate::code::scope::ScopeId;
use crate::code::symbol::Segment;
use crate::code::syntax::text_of;

use super::items::What;
use super::{Visit, Walk, marks};

/// A leaf of a `pub use`: its container, its name, its bytes and what it
/// is.
pub(super) struct Reexport {
    container: Option<ScopeId>,
    name: Segment,
    range: Range<usize>,
    what: What,
}

impl Walk<'_> {
    /// Hold the leaves of a `use` with a visibility, each spanning the leaf,
    /// for the end of the walk.
    pub(super) fn hold_reexports(&mut self, visit: Visit<'_>) {
        let Visit { node, context, .. } = visit;
        let visibility = marks::visibility_of(node, self.text);
        let Some(argument) = node
            .child_by_field_name("argument")
            .filter(|_| visibility != Visibility::Private)
        else {
            return;
        };
        for (name, leaf) in use_leaves(argument, self.text) {
            self.reexports.push(Reexport {
                container: context.scope,
                name: Segment::name(&name),
                range: leaf.start_byte()..leaf.end_byte(),
                what: What {
                    kind: DefinitionKind::Reexport,
                    role: context.role,
                    visibility: Some(visibility),
                },
            });
        }
    }

    /// Define each `pub use` leaf whose path the file does not define
    /// itself (`pub(crate) use name;` after `macro_rules! name`).
    pub(super) fn define_reexports(&mut self) -> Result<(), IndexFault> {
        let defined: BTreeSet<(Option<ScopeId>, &[Segment])> = self
            .index
            .definitions
            .iter()
            .filter_map(|d| self.scopes.get(d.scope))
            .map(|scope| (scope.parent, scope.name.as_slice()))
            .collect();
        let new: Vec<Reexport> = std::mem::take(&mut self.reexports)
            .into_iter()
            .filter(|leaf| !defined.contains(&(leaf.container, std::slice::from_ref(&leaf.name))))
            .collect();
        for leaf in new {
            let scope = self.scopes.enter(leaf.container, vec![leaf.name])?;
            self.define(scope, leaf.range, leaf.what)?;
        }
        Ok(())
    }
}

/// The names a `use` tree brings in, each with the node it is spelled at:
/// a path's last name, an alias, and the module of a `self` in a list;
/// not a glob, nor an alias `_`.
fn use_leaves<'n>(argument: Node<'n>, text: &str) -> Vec<(String, Node<'n>)> {
    let mut found = Vec::new();
    let mut stack = vec![(argument, None)];
    while let Some((node, module)) = stack.pop() {
        let field = |name| node.child_by_field_name(name);
        match node.kind() {
            "identifier" => found.push((text_of(node, text).to_owned(), node)),
            "scoped_identifier" => {
                if let Some(name) = field("name") {
                    found.push((text_of(name, text).to_owned(), node));
                }
            }
            "use_as_clause" => {
                if let Some(alias) = field("alias").filter(|a| text_of(*a, text) != "_") {
                    found.push((text_of(alias, text).to_owned(), node));
                }
            }
            "self" => {
                if let Some(module) = module {
                    found.push((module, node));
                }
            }
            "scoped_use_list" => {
                let module = field("path").map(|path| last_name(path, text));
                if let Some(list) = field("list") {
                    stack.push((list, module));
                }
            }
            "use_list" => {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    // clone: each entry of the list may be a `self`.
                    stack.push((child, module.clone()));
                }
            }
            _ => {}
        }
    }
    found
}

/// The last name of a path (`b` of `a::b`).
fn last_name(path: Node<'_>, text: &str) -> String {
    let name = path.child_by_field_name("name").unwrap_or(path);
    text_of(name, text).to_owned()
}
