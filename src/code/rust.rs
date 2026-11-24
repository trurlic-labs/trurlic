//! Rust definitions from tree-sitter-rust's tree: items, the fields of a
//! struct or union, the variants of an enum with their fields, the leaves
//! of a `pub use`, and what a trait or an `impl` holds, each named by its
//! path from the outermost container.
//!
//! A container gives what it holds a segment: a `mod`, a type, a trait,
//! an enum variant, a function (an item in its body is `outer::inner`),
//! a `const` or `static` (an item in its initializer). An inherent `impl`
//! gives its items the type's segment, its generics, path and references
//! dropped (`impl<T> a::Table<T>` and `impl &Table` are `Table`), and is no
//! definition itself; a trait impl adds the trait's segment
//! (`Table::<Display>::fmt`) and is the definition of that path. A tuple
//! field has no name and defines nothing; neither does a `use` without a
//! visibility, a glob, an item named `_`, or a `pub use` of a path the file
//! already defines (`pub(crate) use name;` after `macro_rules! name`). An
//! item's span takes in the attributes and doc comments before it.
//!
//! A macro invocation where an item or a statement can stand is read by
//! the lexical rule of `macros.rs`: an item it spells (`define! { struct
//! Name { .. } }`, `proptest! { fn case() {} }`) is a definition under the
//! invocation's container, and the invocation is kept for the expansion of
//! the crate's type-defining macros (`expand.rs`).

mod macros;

pub(super) use macros::is_word;

use std::collections::BTreeSet;

use tree_sitter::Node;

use super::index::{Definition, FileIndex, IndexFault, Invocation, MAX_NESTING};
use super::span::Lines;
use super::symbol::Segment;
use super::syntax::{self, Leaves, text_of};

/// The index of `text`, a Rust file.
pub(super) fn index(text: &str) -> Result<FileIndex, IndexFault> {
    let tree = syntax::parse(text, &tree_sitter_rust::LANGUAGE.into())?;
    let mut walk = Walk {
        text,
        leaves: Leaves::of(&tree),
        lines: Lines::of(text.as_bytes()),
        index: FileIndex::default(),
        reexports: Vec::new(),
        scopes: vec![Vec::new()],
        too_deep: false,
    };
    walk.run(tree.root_node());
    if walk.too_deep {
        return Err(IndexFault::TooDeep);
    }
    let mut index = walk.index;
    let defined: BTreeSet<&[Segment]> = index
        .definitions
        .iter()
        .map(|d| d.path.as_slice())
        .collect();
    let new_names: Vec<Definition> = walk
        .reexports
        .into_iter()
        .filter(|reexport| !defined.contains(&reexport.path.as_slice()))
        .collect();
    index.definitions.extend(new_names);
    Ok(index)
}

/// One walk of a tree, which visits each node once, from an explicit
/// stack: a nesting as deep as the file allows costs no call stack.
struct Walk<'t> {
    text: &'t str,
    leaves: Leaves,
    lines: Lines,
    index: FileIndex,
    /// The leaves of the `pub use`s, defined after the walk, once the
    /// paths of the file's items are all found.
    reexports: Vec<Definition>,
    /// The path of each container met so far; a node is walked with the
    /// index of the one it is in.
    scopes: Vec<Vec<Segment>>,
    /// A container nested past [`MAX_NESTING`] was met: the walk stops.
    too_deep: bool,
}

/// The items that define their `name` and may hold more: a function's
/// body, a type's fields, a `const`'s initializer.
const NAMED_CONTAINERS: [&str; 12] = [
    "function_item",
    "function_signature_item",
    "struct_item",
    "union_item",
    "enum_item",
    "trait_item",
    "mod_item",
    "const_item",
    "static_item",
    "type_item",
    "associated_type",
    "enum_variant",
];

/// Nodes whose tokens are no items: an attribute's, a macro body's, a
/// comment's.
const HOLD_NO_ITEMS: [&str; 5] = [
    "attribute_item",
    "inner_attribute_item",
    "token_tree",
    "line_comment",
    "block_comment",
];

/// A node to visit: the scope it is in, where it starts with the
/// attributes and doc comments before it, and its parent's kind. Both are
/// known from the parent's walk of its children: asking a node for its
/// parent or its previous sibling costs a walk of its parent's children.
#[derive(Clone, Copy)]
struct Visit<'n> {
    node: Node<'n>,
    scope: usize,
    start: usize,
    parent: &'n str,
}

impl Walk<'_> {
    fn run(&mut self, root: Node<'_>) {
        let mut stack = vec![Visit {
            node: root,
            scope: 0,
            start: root.start_byte(),
            parent: "",
        }];
        while let Some(visit) = stack.pop().filter(|_| !self.too_deep) {
            if let Some(inner) = self.visit(visit) {
                push_children(&mut stack, visit.node, inner);
            }
        }
    }

    /// Record what `visit`'s node defines; the scope its children are
    /// walked in, or `None` when they hold no item.
    fn visit(&mut self, visit: Visit<'_>) -> Option<usize> {
        let Visit { node, scope, .. } = visit;
        match node.kind() {
            kind if NAMED_CONTAINERS.contains(&kind) => Some(self.define_named(visit)),
            "field_declaration" => {
                self.define_named(visit);
                None
            }
            "impl_item" => Some(self.enter_impl(visit)),
            "macro_definition" => {
                self.define_macro(visit);
                None
            }
            "use_declaration" => {
                self.define_reexports(node, scope);
                None
            }
            "macro_invocation" => {
                self.read_invocation(visit, visit);
                None
            }
            "expression_statement" => match node.named_child(0) {
                Some(invocation) if invocation.kind() == "macro_invocation" => {
                    let inner = Visit {
                        node: invocation,
                        ..visit
                    };
                    self.read_invocation(inner, visit);
                    None
                }
                Some(_) | None => Some(scope),
            },
            kind if HOLD_NO_ITEMS.contains(&kind) => None,
            // Every other node, an expression or a block, may hold items.
            _ => Some(scope),
        }
    }

    /// Define the item `node` under its `name`, and return the scope of
    /// what it holds.
    fn define_named(&mut self, visit: Visit<'_>) -> usize {
        let Some(name) = visit.node.child_by_field_name("name") else {
            return visit.scope;
        };
        let name = text_of(name, self.text);
        let inner = self.enter(visit.scope, [Segment::name(name)]);
        if name != "_" {
            let definition = self.definition(inner, visit.start, visit.node.end_byte());
            self.index.definitions.push(definition);
        }
        inner
    }

    /// Define a `macro_rules!` macro, and keep its facts when it can define
    /// a type.
    fn define_macro(&mut self, visit: Visit<'_>) {
        self.define_named(visit);
        let attributes = self.text.get(visit.start..visit.node.start_byte());
        let exported = attributes.is_some_and(|text| text.contains("macro_export"));
        if let Some(found) = macros::type_macro(visit.node, self.text, exported) {
            self.index.macros.push(found);
        }
    }

    /// An `impl`'s scope: the type's segment, then the trait's for a trait
    /// impl, which is itself a definition.
    fn enter_impl(&mut self, visit: Visit<'_>) -> usize {
        let Visit {
            node, scope, start, ..
        } = visit;
        let Some(self_type) = node.child_by_field_name("type") else {
            return scope;
        };
        let type_segment = type_segment(self_type, self.text);
        match node.child_by_field_name("trait") {
            Some(trait_node) => {
                let trait_segment = Segment::of_trait(text_of(trait_node, self.text));
                let inner = self.enter(scope, [type_segment, trait_segment]);
                let definition = self.definition(inner, start, node.end_byte());
                self.index.definitions.push(definition);
                inner
            }
            None => self.enter(scope, [type_segment]),
        }
    }

    /// Hold the leaves of a `use` with a visibility, each spanning the leaf,
    /// for the end of the walk.
    fn define_reexports(&mut self, node: Node<'_>, scope: usize) {
        let mut cursor = node.walk();
        let visible = node
            .named_children(&mut cursor)
            .any(|child| child.kind() == "visibility_modifier");
        let Some(argument) = node.child_by_field_name("argument").filter(|_| visible) else {
            return;
        };
        for (name, leaf) in use_leaves(argument, self.text) {
            let inner = self.enter(scope, [Segment::name(&name)]);
            let definition = self.definition(inner, leaf.start_byte(), leaf.end_byte());
            self.reexports.push(definition);
        }
    }

    /// Define the items an invocation where an item or a statement can
    /// stand spells, and keep it for the expansion of the crate's macros.
    /// `outer` is the invocation, or the statement that holds it.
    fn read_invocation(&mut self, invocation: Visit<'_>, outer: Visit<'_>) {
        if !matches!(outer.parent, "source_file" | "declaration_list" | "block") {
            return;
        }
        let (node, scope) = (invocation.node, invocation.scope);
        for (name, start, end) in macros::spelled_items(node, self.text) {
            let inner = self.enter(scope, [Segment::name(&name)]);
            let definition = self.definition(inner, start, end);
            self.index.definitions.push(definition);
        }
        // A type-defining macro is invoked where an item stands.
        if !matches!(outer.parent, "source_file" | "declaration_list") {
            return;
        }
        let Some((macro_name, tokens)) = macros::invocation_tokens(node, self.text) else {
            return;
        };
        let (start, end) = (outer.start, outer.node.end_byte());
        self.index.invocations.push(Invocation {
            macro_name,
            container: self.scope_path(scope).to_vec(),
            span: self.lines.span(start, end),
            hash: self.leaves.hash(self.text, start, end),
            tokens,
        });
    }

    /// The scope of `segments` inside `scope`.
    fn enter(&mut self, scope: usize, segments: impl IntoIterator<Item = Segment>) -> usize {
        let mut path = self.scope_path(scope).to_vec();
        path.extend(segments);
        if path.len() > MAX_NESTING {
            self.too_deep = true;
            return scope;
        }
        self.scopes.push(path);
        self.scopes.len() - 1
    }

    /// The definition of the path of `scope` over bytes `start..end`.
    fn definition(&self, scope: usize, start: usize, end: usize) -> Definition {
        Definition {
            path: self.scope_path(scope).to_vec(),
            span: self.lines.span(start, end),
            hash: self.leaves.hash(self.text, start, end),
        }
    }

    fn scope_path(&self, scope: usize) -> &[Segment] {
        self.scopes.get(scope).map_or(&[], Vec::as_slice)
    }
}

/// Push the named children of `node` to visit in `scope`, each with where
/// it starts: at the first of the attributes and outer doc comments right
/// before it.
fn push_children<'n>(stack: &mut Vec<Visit<'n>>, node: Node<'n>, scope: usize) {
    let mut cursor = node.walk();
    let mut leading = None;
    for child in node.named_children(&mut cursor) {
        let attached = child.kind() == "attribute_item"
            || (matches!(child.kind(), "line_comment" | "block_comment")
                && child.child_by_field_name("outer").is_some());
        let start = leading.unwrap_or(child.start_byte());
        leading = attached.then_some(start);
        stack.push(Visit {
            node: child,
            scope,
            start,
            parent: node.kind(),
        });
    }
}

/// The segment an `impl` gives its type: the last name of its path, its
/// generics and references dropped; a type with no name (a tuple, an
/// array) is its text with the spaces removed.
fn type_segment(mut node: Node<'_>, text: &str) -> Segment {
    loop {
        let inner = match node.kind() {
            "generic_type" | "reference_type" | "pointer_type" => node.child_by_field_name("type"),
            "scoped_type_identifier" => node.child_by_field_name("name"),
            "dynamic_type" => node.child_by_field_name("trait"),
            _ => None,
        };
        match inner {
            Some(inner) => node = inner,
            None => break,
        }
    }
    let name = text_of(node, text);
    match node.kind() {
        "type_identifier" | "primitive_type" | "identifier" => Segment::name(name),
        _ => Segment::Name(name.chars().filter(|c| !c.is_whitespace()).collect()),
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

#[cfg(test)]
mod tests;
