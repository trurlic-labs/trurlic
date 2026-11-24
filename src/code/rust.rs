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

mod invocations;
mod items;
mod macros;
mod marks;
mod reexports;
mod spelled;

pub(super) use macros::is_word;

use std::ops::Range;

use tree_sitter::Node;

use super::index::{Definition, FileIndex, IndexFault, ModuleFile};
use super::kinds::{DefinitionKind, Role, Visibility};
use super::scope::{ScopeId, ScopeTree};
use super::span::Lines;
use super::symbol::Segment;
use super::syntax::{self, Leaves, text_of};
use super::work::Work;

use items::{Item, What, Within};
use marks::Marks;

/// The index of `text`, a Rust file.
pub(super) fn index(text: &str) -> Result<FileIndex, IndexFault> {
    let tree = syntax::parse(text, &tree_sitter_rust::LANGUAGE.into())?;
    let mut walk = Walk {
        text,
        leaves: Leaves::of(&tree),
        lines: Lines::of(text.as_bytes()),
        scopes: ScopeTree::default(),
        index: FileIndex::default(),
        reexports: Vec::new(),
        work: Work::of(text.len()),
    };
    walk.run(tree.root_node())?;
    walk.define_reexports()?;
    let mut index = walk.index;
    index.scopes = walk.scopes.into_scopes();
    Ok(index)
}

/// One walk of a tree, which visits each node once, from an explicit
/// stack: a nesting as deep as the file allows costs no call stack.
struct Walk<'t> {
    text: &'t str,
    leaves: Leaves,
    lines: Lines,
    scopes: ScopeTree,
    /// The definitions, macros and invocations found; its scopes are
    /// [`Walk::scopes`] until the walk ends.
    index: FileIndex,
    /// The leaves of the `pub use`s, defined after the walk, once the
    /// paths of the file's items are all found.
    reexports: Vec<reexports::Reexport>,
    work: Work,
}

/// Nodes whose tokens are no items: an attribute's, a macro body's, a
/// comment's.
const HOLD_NO_ITEMS: [&str; 5] = [
    "attribute_item",
    "inner_attribute_item",
    "token_tree",
    "line_comment",
    "block_comment",
];

/// What a node's children are walked in: the scope of their path (`None`
/// at the file's top), the container they stand in, and whether they are
/// test code.
#[derive(Clone, Copy)]
struct Context {
    scope: Option<ScopeId>,
    within: Within,
    role: Role,
}

/// A node to visit: its context, where it starts with the attributes and
/// doc comments before it, its parent's kind, and whether an attribute
/// exports it as a macro. All are known from the parent's walk of its
/// children: asking a node for its parent or its previous sibling costs a
/// walk of its parent's children. Its role takes in its own attributes.
#[derive(Clone, Copy)]
struct Visit<'n> {
    node: Node<'n>,
    context: Context,
    start: usize,
    parent: &'n str,
    /// What its attributes say: its role is in its context.
    marks: Marks<'n>,
}

impl Walk<'_> {
    fn run(&mut self, root: Node<'_>) -> Result<(), IndexFault> {
        let mut stack = vec![Visit {
            node: root,
            context: Context {
                scope: None,
                within: Within::Items,
                role: Role::Code,
            },
            start: root.start_byte(),
            parent: "",
            marks: Marks {
                role: Role::Code,
                macro_export: false,
                path: None,
            },
        }];
        while let Some(visit) = stack.pop() {
            if let Some(inner) = self.visit(visit)? {
                push_children(&mut stack, visit.node, inner, self.text);
            }
        }
        Ok(())
    }

    /// Record what `visit`'s node defines; the context its children are
    /// walked in, or `None` when they hold no item.
    fn visit(&mut self, visit: Visit<'_>) -> Result<Option<Context>, IndexFault> {
        let Visit { node, context, .. } = visit;
        if let Some(item) = Item::of_node(node.kind()) {
            let inner = self.define_named(visit, item)?;
            return Ok(item.holds_items().then_some(inner));
        }
        Ok(match node.kind() {
            "impl_item" => Some(self.enter_impl(visit)?),
            "macro_definition" => {
                self.define_macro(visit)?;
                None
            }
            "use_declaration" => {
                self.hold_reexports(visit);
                None
            }
            "macro_invocation" => {
                self.read_invocation(visit, visit)?;
                None
            }
            "expression_statement" => match node.named_child(0) {
                Some(invocation) if invocation.kind() == "macro_invocation" => {
                    let inner = Visit {
                        node: invocation,
                        ..visit
                    };
                    self.read_invocation(inner, visit)?;
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
        })
    }

    /// Define the item `node` under its `name`, and return the context of
    /// what it holds. An inner attribute of its body (`mod m {
    /// #![cfg(test)] }`) is the item's.
    fn define_named(&mut self, visit: Visit<'_>, item: Item) -> Result<Context, IndexFault> {
        let Visit {
            node, mut context, ..
        } = visit;
        if let Some(body) = node.child_by_field_name("body") {
            context.role = marks::inner_role(body, context.role, self.text);
        }
        let own = marks::visibility_of(node, self.text);
        let (kind, inner_within) = item.kind(context.within, own);
        let Some(name) = node.child_by_field_name("name") else {
            return Ok(context);
        };
        let name = text_of(name, self.text);
        let scope = self
            .scopes
            .enter(context.scope, vec![Segment::name(name)])?;
        if name != "_" {
            let what = What {
                kind,
                role: context.role,
                visibility: context.within.visibility(own),
            };
            self.define(scope, visit.start..node.end_byte(), what)?;
        }
        if item == Item::Module && node.child_by_field_name("body").is_none() {
            self.index.module_files.push(ModuleFile {
                scope,
                role: context.role,
                path: visit
                    .marks
                    .path
                    .map(|literal| string_value(literal, self.text)),
            });
        }
        Ok(Context {
            scope: Some(scope),
            within: inner_within,
            role: context.role,
        })
    }

    /// Define a `macro_rules!` macro, and keep its facts when it can define
    /// a type. A macro is private to its module and those below unless an
    /// attribute exports it.
    fn define_macro(&mut self, visit: Visit<'_>) -> Result<(), IndexFault> {
        let Some(name) = visit.node.child_by_field_name("name") else {
            return Ok(());
        };
        let name = vec![Segment::name(text_of(name, self.text))];
        let scope = self.scopes.enter(visit.context.scope, name)?;
        let what = What {
            kind: DefinitionKind::Macro,
            role: visit.context.role,
            visibility: Some(match visit.marks.macro_export {
                true => Visibility::Public,
                false => Visibility::Private,
            }),
        };
        self.define(scope, visit.start..visit.node.end_byte(), what)?;
        let exported = visit.marks.macro_export;
        let found = macros::type_macro(visit.node, self.text, exported, &mut self.work)?;
        self.index.macros.extend(found);
        Ok(())
    }

    /// An `impl`'s context: the type's segment, then the trait's for a
    /// trait impl, which is itself a definition.
    fn enter_impl(&mut self, visit: Visit<'_>) -> Result<Context, IndexFault> {
        let Visit {
            node,
            context,
            start,
            ..
        } = visit;
        let Some(self_type) = node.child_by_field_name("type") else {
            return Ok(context);
        };
        let type_segment = type_segment(self_type, self.text);
        Ok(match node.child_by_field_name("trait") {
            Some(trait_node) => {
                let trait_segment = Segment::of_trait(text_of(trait_node, self.text));
                let scope = self
                    .scopes
                    .enter(context.scope, vec![type_segment, trait_segment])?;
                let what = What {
                    kind: DefinitionKind::Impl,
                    role: context.role,
                    visibility: None,
                };
                self.define(scope, start..node.end_byte(), what)?;
                Context {
                    scope: Some(scope),
                    within: Within::TraitImpl,
                    role: context.role,
                }
            }
            None => Context {
                scope: Some(self.scopes.enter(context.scope, vec![type_segment])?),
                within: Within::Impl,
                role: context.role,
            },
        })
    }

    /// Define `scope` over bytes `range` as `what` says, its hash's tokens
    /// spent from the budget.
    fn define(
        &mut self,
        scope: ScopeId,
        range: Range<usize>,
        what: What,
    ) -> Result<(), IndexFault> {
        let hash = self.hash(range.start..range.end)?;
        self.index.definitions.push(Definition {
            scope,
            kind: what.kind,
            span: self.lines.span(range.start, range.end),
            hash,
            role: what.role,
            visibility: what.visibility,
        });
        Ok(())
    }

    /// The hash of the tokens within bytes `range`, each spent from the
    /// budget.
    fn hash(&mut self, range: Range<usize>) -> Result<super::span::SpanHash, IndexFault> {
        let (hash, tokens) = self.leaves.hash(self.text, range.start, range.end);
        self.work.spend(tokens)?;
        Ok(hash)
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
    let mut leading: Option<(usize, Marks<'n>)> = None;
    for child in children {
        let attribute = child.kind() == "attribute_item";
        let comment = matches!(child.kind(), "line_comment" | "block_comment");
        let doc = comment && child.child_by_field_name("outer").is_some();
        // A plain comment between an attribute and its item is neither:
        // the attributes before it still mark the item after it.
        if comment && !doc && leading.is_some() {
            continue;
        }
        let attached = attribute || doc;
        let unmarked = Marks {
            role: inner_role,
            macro_export: false,
            path: None,
        };
        let (start, mut marks) = leading.unwrap_or((child.start_byte(), unmarked));
        if attribute {
            marks = marks.and(marks::attribute_item_marks(child, text));
        }
        leading = attached.then_some((start, marks));
        // A variant's discriminant is an expression: an item in it is
        // visible as its own modifier says, not as the enum.
        let within = match node.kind() == "enum_variant"
            && !child.kind().ends_with("field_declaration_list")
        {
            true => Within::Items,
            false => context.within,
        };
        stack.push(Visit {
            node: child,
            context: Context {
                role: marks.role,
                within,
                ..context
            },
            start,
            parent: node.kind(),
            marks,
        });
    }
}

/// The text a string literal holds, between its quotes; escapes are kept
/// as written, which a path rarely holds.
fn string_value(literal: Node<'_>, text: &str) -> String {
    let spelled = text_of(literal, text);
    let inside = spelled
        .find('"')
        .zip(spelled.rfind('"'))
        .and_then(|(first, last)| spelled.get(first + 1..last));
    inside.unwrap_or_default().to_owned()
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

#[cfg(test)]
mod tests;
