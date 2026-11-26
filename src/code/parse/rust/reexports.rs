//! The leaves of a `pub use`: each name it brings in is a re-export of
//! the module it stands in, unless the file defines that path itself
//! (`pub(crate) use name;` after `macro_rules! name`), which is known once
//! the walk has found every item.
//!
//! A leaf is held as the bytes of its name, never a copy, and a name one
//! module re-exports twice (`{self, self}`, `{a, a}`) is defined once,
//! at its first leaf: what an index holds of the `pub use`s is at most
//! one scope per distinct name, each charged to the file's room.

use std::collections::BTreeSet;
use std::ops::Range;

use tree_sitter::Node;

use super::marks;
use crate::code::index::{DefinitionKind, IndexFault, Role, ScopeId, Segment, Visibility, What};
use crate::code::parse::parser::text_of;
use crate::code::parse::tree_index::TreeIndex;

/// A leaf of a `pub use`: its container, the bytes of its name, its own
/// bytes and what it is.
pub(super) struct Reexport {
    container: Option<ScopeId>,
    name: Range<usize>,
    range: Range<usize>,
    what: What,
}

/// The leaves of the `use` at `node`, standing in the module `container`
/// with the role `role`, when it has a visibility: each spans its leaf,
/// held for the end of the walk.
pub(super) fn held(
    node: Node<'_>,
    container: Option<ScopeId>,
    role: Role,
    text: &str,
) -> Vec<Reexport> {
    let visibility = marks::visibility_of(node, text);
    let Some(argument) = node
        .child_by_field_name("argument")
        .filter(|_| visibility != Visibility::Private)
    else {
        return Vec::new();
    };
    let what = What {
        kind: DefinitionKind::Reexport,
        role,
        visibility: Some(visibility),
    };
    use_leaves(argument, text)
        .into_iter()
        .map(|(name, leaf)| Reexport {
            container,
            name: name.byte_range(),
            range: leaf.byte_range(),
            what,
        })
        .collect()
}

/// Define in `build` each of `leaves`, the `pub use` leaves of `text`,
/// once per container and name, whose path the file does not define
/// itself.
pub(super) fn define(
    mut leaves: Vec<Reexport>,
    build: &mut TreeIndex<'_>,
    text: &str,
) -> Result<(), IndexFault> {
    leaves.sort_by_key(|leaf| leaf.range.start);
    // The same name node, a `self` list's module, first; then the
    // texts of the names left, each read once.
    let mut nodes = BTreeSet::new();
    let mut names: BTreeSet<(Option<ScopeId>, &str)> = BTreeSet::new();
    let distinct: Vec<Reexport> = leaves
        .into_iter()
        .filter(|leaf| nodes.insert((leaf.container, leaf.name.start, leaf.name.end)))
        .filter(|leaf| {
            names.insert((
                leaf.container,
                text.get(leaf.name.start..leaf.name.end).unwrap_or_default(),
            ))
        })
        .collect();
    let defined: BTreeSet<(Option<ScopeId>, &[Segment])> = build
        .definitions()
        .iter()
        .filter_map(|d| build.scopes.get(d.scope))
        .map(|scope| (scope.parent, scope.name.as_slice()))
        .collect();
    let new: Vec<(Reexport, Segment)> = distinct
        .into_iter()
        .map(|leaf| {
            let name =
                Segment::of_rust(text.get(leaf.name.start..leaf.name.end).unwrap_or_default());
            (leaf, name)
        })
        .filter(|(leaf, name)| !defined.contains(&(leaf.container, std::slice::from_ref(name))))
        .collect();
    for (leaf, name) in new {
        let scope = build.scopes.enter(leaf.container, vec![name])?;
        build.define(scope, leaf.range, leaf.what)?;
    }
    Ok(())
}

/// The names a `use` tree brings in, each with the node it is spelled at:
/// a path's last name, an alias, and the module of a `self` in a list;
/// not a glob, nor an alias `_`. A list's entries carry its module as a
/// node, never a copy of its text.
fn use_leaves<'n>(argument: Node<'n>, text: &str) -> Vec<(Node<'n>, Node<'n>)> {
    let mut found = Vec::new();
    let mut stack: Vec<(Node<'n>, Option<Node<'n>>)> = vec![(argument, None)];
    while let Some((node, module)) = stack.pop() {
        let field = |name| node.child_by_field_name(name);
        match node.kind() {
            "identifier" => found.push((node, node)),
            "scoped_identifier" => {
                if let Some(name) = field("name") {
                    found.push((name, node));
                }
            }
            "use_as_clause" => {
                if let Some(alias) = field("alias").filter(|a| text_of(*a, text) != "_") {
                    found.push((alias, node));
                }
            }
            "self" => {
                if let Some(module) = module {
                    found.push((module, node));
                }
            }
            "scoped_use_list" => {
                let module = field("path").map(last_name);
                if let Some(list) = field("list") {
                    stack.push((list, module));
                }
            }
            "use_list" => {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    stack.push((child, module));
                }
            }
            _ => {}
        }
    }
    found
}

/// The node of the last name of a path (`b` of `a::b`).
fn last_name(path: Node<'_>) -> Node<'_> {
    path.child_by_field_name("name").unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use crate::code::index::FileKind;
    use crate::code::testing;

    #[test]
    fn a_reexport_of_a_path_the_file_defines_is_no_second_definition() {
        let text = "macro_rules! m { () => {} }\npub(crate) use m;\npub use inner::Thing;\nmod inner { pub struct Thing; }\npub use other::{self as renamed, *};\nuse private::Name;\npub use x::y as _;\n";
        let paths: Vec<String> = testing::defined(FileKind::Rust, text)
            .into_iter()
            .map(|(path, _)| path)
            .collect();

        assert_eq!(paths, ["m", "Thing", "inner", "inner::Thing", "renamed"]);
    }
}
