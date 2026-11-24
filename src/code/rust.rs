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

mod items;
mod macros;
mod marks;

pub(super) use macros::is_word;

use std::collections::BTreeSet;
use std::ops::Range;

use tree_sitter::Node;

use super::index::{Definition, FileIndex, IndexFault, Invocation, MAX_NESTING};
use super::kinds::{DefinitionKind, Role, Visibility};
use super::span::Lines;
use super::symbol::Segment;
use super::syntax::{self, Leaves, text_of};

use items::{What, Within, item_kind};

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

/// What a node's children are walked in: the scope of their path, the
/// container they stand in, and whether they are test code.
#[derive(Clone, Copy)]
struct Context {
    scope: usize,
    within: Within,
    role: Role,
}

/// A node to visit: its context, where it starts with the attributes and
/// doc comments before it, and its parent's kind. Both are known from
/// the parent's walk of its children: asking a node for its parent or its
/// previous sibling costs a walk of its parent's children. Its role
/// takes in its own attributes.
#[derive(Clone, Copy)]
struct Visit<'n> {
    node: Node<'n>,
    context: Context,
    start: usize,
    parent: &'n str,
}

impl Walk<'_> {
    fn run(&mut self, root: Node<'_>) {
        let mut stack = vec![Visit {
            node: root,
            context: Context {
                scope: 0,
                within: Within::Items,
                role: Role::Code,
            },
            start: root.start_byte(),
            parent: "",
        }];
        while let Some(visit) = stack.pop().filter(|_| !self.too_deep) {
            if let Some(inner) = self.visit(visit) {
                push_children(&mut stack, visit.node, inner, self.text);
            }
        }
    }

    /// Record what `visit`'s node defines; the context its children are
    /// walked in, or `None` when they hold no item.
    fn visit(&mut self, visit: Visit<'_>) -> Option<Context> {
        let Visit { node, context, .. } = visit;
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
                self.define_reexports(visit);
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
                Some(_) | None => Some(context),
            },
            "foreign_mod_item" => Some(Context {
                within: Within::Items,
                ..context
            }),
            kind if HOLD_NO_ITEMS.contains(&kind) => None,
            // Every other node, an expression or a block, may hold items.
            _ => Some(context),
        }
    }

    /// Define the item `node` under its `name`, and return the context of
    /// what it holds. An inner attribute of its body (`mod m {
    /// #![cfg(test)] }`) is the item's.
    fn define_named(&mut self, visit: Visit<'_>) -> Context {
        let Visit {
            node, mut context, ..
        } = visit;
        if let Some(body) = node.child_by_field_name("body") {
            context.role = marks::inner_role(body, context.role, self.text);
        }
        let own = marks::visibility_of(node, self.text);
        let (kind, inner_within) = item_kind(node.kind(), context.within, own);
        let Some(name) = node.child_by_field_name("name") else {
            return context;
        };
        let name = text_of(name, self.text);
        let scope = self.enter(context.scope, [Segment::name(name)]);
        if name != "_" {
            let what = What {
                kind,
                role: context.role,
                visibility: context.within.visibility(own),
            };
            let definition = self.definition(scope, visit.start..node.end_byte(), what);
            self.index.definitions.push(definition);
        }
        Context {
            scope,
            within: inner_within,
            role: context.role,
        }
    }

    /// Define a `macro_rules!` macro, and keep its facts when it can define
    /// a type. A macro is private to its module and those below unless it
    /// is exported.
    fn define_macro(&mut self, visit: Visit<'_>) {
        let attributes = self.text.get(visit.start..visit.node.start_byte());
        let exported = attributes.is_some_and(|text| text.contains("macro_export"));
        let Some(name) = visit.node.child_by_field_name("name") else {
            return;
        };
        let scope = self.enter(
            visit.context.scope,
            [Segment::name(text_of(name, self.text))],
        );
        let what = What {
            kind: DefinitionKind::Macro,
            role: visit.context.role,
            visibility: Some(match exported {
                true => Visibility::Public,
                false => Visibility::Private,
            }),
        };
        let definition = self.definition(scope, visit.start..visit.node.end_byte(), what);
        self.index.definitions.push(definition);
        if let Some(found) = macros::type_macro(visit.node, self.text, exported) {
            self.index.macros.push(found);
        }
    }

    /// An `impl`'s context: the type's segment, then the trait's for a
    /// trait impl, which is itself a definition.
    fn enter_impl(&mut self, visit: Visit<'_>) -> Context {
        let Visit {
            node,
            context,
            start,
            ..
        } = visit;
        let Some(self_type) = node.child_by_field_name("type") else {
            return context;
        };
        let type_segment = type_segment(self_type, self.text);
        match node.child_by_field_name("trait") {
            Some(trait_node) => {
                let trait_segment = Segment::of_trait(text_of(trait_node, self.text));
                let scope = self.enter(context.scope, [type_segment, trait_segment]);
                let what = What {
                    kind: DefinitionKind::Impl,
                    role: context.role,
                    visibility: None,
                };
                let definition = self.definition(scope, start..node.end_byte(), what);
                self.index.definitions.push(definition);
                Context {
                    scope,
                    within: Within::TraitImpl,
                    role: context.role,
                }
            }
            None => Context {
                scope: self.enter(context.scope, [type_segment]),
                within: Within::Impl,
                role: context.role,
            },
        }
    }

    /// Hold the leaves of a `use` with a visibility, each spanning the leaf,
    /// for the end of the walk.
    fn define_reexports(&mut self, visit: Visit<'_>) {
        let Visit { node, context, .. } = visit;
        let visibility = marks::visibility_of(node, self.text);
        let Some(argument) = node
            .child_by_field_name("argument")
            .filter(|_| visibility != Visibility::Private)
        else {
            return;
        };
        for (name, leaf) in use_leaves(argument, self.text) {
            let scope = self.enter(context.scope, [Segment::name(&name)]);
            let what = What {
                kind: DefinitionKind::Reexport,
                role: context.role,
                visibility: Some(visibility),
            };
            let definition = self.definition(scope, leaf.start_byte()..leaf.end_byte(), what);
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
        let (node, context) = (invocation.node, invocation.context);
        for item in macros::spelled_items(node, self.text) {
            let Some(kind) = DefinitionKind::of_keyword(item.keyword, context.within.container())
            else {
                continue;
            };
            let scope = self.enter(context.scope, [Segment::name(item.name)]);
            let what = What {
                kind,
                role: context.role.or(item.role),
                visibility: context.within.visibility(item.visibility),
            };
            let definition = self.definition(scope, item.start..item.end, what);
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
            container: self.scope_path(context.scope).to_vec(),
            span: self.lines.span(start, end),
            hash: self.leaves.hash(self.text, start, end),
            tokens,
            role: context.role,
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

    /// The definition of the path of `scope` over bytes `range`.
    fn definition(&self, scope: usize, range: Range<usize>, what: What) -> Definition {
        Definition {
            path: self.scope_path(scope).to_vec(),
            kind: what.kind,
            span: self.lines.span(range.start, range.end),
            hash: self.leaves.hash(self.text, range.start, range.end),
            role: what.role,
            visibility: what.visibility,
        }
    }

    fn scope_path(&self, scope: usize) -> &[Segment] {
        self.scopes.get(scope).map_or(&[], Vec::as_slice)
    }
}

/// Push the named children of `node` to visit in `context`, each with
/// where it starts: at the first of the attributes and outer doc comments
/// right before it; and each with the role its attributes give it, and
/// an inner `#![cfg(test)]` gives every child.
fn push_children<'n>(stack: &mut Vec<Visit<'n>>, node: Node<'n>, context: Context, text: &str) {
    let inner_role = marks::inner_role(node, context.role, text);
    let mut cursor = node.walk();
    let children: Vec<Node<'n>> = node.named_children(&mut cursor).collect();
    let mut leading: Option<(usize, Role)> = None;
    for child in children {
        let attribute = child.kind() == "attribute_item";
        let attached = attribute
            || (matches!(child.kind(), "line_comment" | "block_comment")
                && child.child_by_field_name("outer").is_some());
        let (start, role) = leading.unwrap_or((child.start_byte(), inner_role));
        let role = match attribute {
            true => role.or(marks::attribute_item_role(child, text)),
            false => role,
        };
        leading = attached.then_some((start, role));
        stack.push(Visit {
            node: child,
            context: Context { role, ..context },
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
