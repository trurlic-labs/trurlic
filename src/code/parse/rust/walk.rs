//! The walk of tree-sitter-rust's tree that defines each item it holds,
//! from an explicit stack, each node visited once with the container it
//! stands in, the role its attributes give it and its doc ([`index`]).

use tree_sitter::Node;

use super::docs;
use super::items::{Item, Within};
use super::lexicon::LEXICON;
use super::marks::{self, Marks};
use super::reexports::{self, Reexport};
use super::silent;
use crate::code::index::{
    DefinitionKind, FileIndex, IndexFault, Lines, ModuleFile, Role, ScopeId, ScopeTree, Segment,
    Visibility, What,
};
use crate::code::parse::parser::{self, text_of};
use crate::code::parse::tree_index::TreeIndex;
use crate::code::parse::units::Leaves;

/// The index of `text`, a Rust file, with its doc. Its hashes leave out
/// the tokens of no meaning (`silent.rs`).
pub(in crate::code) fn index(text: &str, scopes: ScopeTree) -> Result<FileIndex, IndexFault> {
    let tree = parser::parse(text, &tree_sitter_rust::LANGUAGE.into())?;
    let silent = silent::silent_tokens(tree.root_node());
    let mut walk = Walk {
        text,
        build: TreeIndex::new(
            text,
            Lines::of(text.as_bytes()),
            Leaves::of(&tree, text, &LEXICON, &silent),
            scopes,
        ),
        reexports: Vec::new(),
    };
    walk.run(tree.root_node())?;
    reexports::define(walk.reexports, &mut walk.build, text)?;
    if let Some(doc) = docs::first_paragraph(&[], Some(tree.root_node()), text) {
        walk.build.document_file(doc)?;
    }
    Ok(walk.build.finish())
}

/// One walk of a tree, which visits each node once, from an explicit
/// stack: a nesting as deep as the file allows costs no call stack.
struct Walk<'t> {
    text: &'t str,
    build: TreeIndex<'t>,
    /// The leaves of the `pub use`s, defined after the walk, once the
    /// paths of the file's items are all found.
    reexports: Vec<Reexport>,
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
/// doc comments before it, what those attributes say, and those nodes.
/// All are known from the parent's walk of its children: asking a node
/// for its previous sibling costs a walk of its parent's children. Its
/// role takes in its own attributes.
struct Visit<'n> {
    node: Node<'n>,
    context: Context,
    start: usize,
    /// What its attributes say: its role is in its context.
    marks: Marks<'n>,
    /// The attributes and outer doc comments before it, in order: the
    /// outer part of its doc.
    attached: Vec<Node<'n>>,
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
            marks: Marks {
                role: Role::Code,
                macro_export: false,
                path: None,
            },
            attached: Vec::new(),
        }];
        while let Some(visit) = stack.pop() {
            let node = visit.node;
            if let Some(inner) = self.visit(&visit)? {
                push_children(&mut stack, node, inner, self.text);
            }
        }
        Ok(())
    }

    /// Record what `visit`'s node defines; the context its children are
    /// walked in, or `None` when they hold no item.
    fn visit(&mut self, visit: &Visit<'_>) -> Result<Option<Context>, IndexFault> {
        let Visit { node, context, .. } = *visit;
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
                let leaves = reexports::held(node, context.scope, context.role, self.text);
                self.reexports.extend(leaves);
                None
            }
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
    fn define_named(&mut self, visit: &Visit<'_>, item: Item) -> Result<Context, IndexFault> {
        let Visit {
            node, mut context, ..
        } = *visit;
        let body = node.child_by_field_name("body");
        if let Some(body) = body {
            context.role = marks::inner_role(body, context.role, self.text);
        }
        let own = marks::visibility_of(node, self.text);
        let (kind, inner_within) = item.kind(context.within, own);
        let Some(name) = node.child_by_field_name("name") else {
            return Ok(context);
        };
        let name = text_of(name, self.text);
        let scope = self
            .build
            .scopes
            .enter(context.scope, vec![Segment::of_rust(name)])?;
        if name != "_" {
            let what = What {
                kind,
                role: context.role,
                visibility: context.within.visibility(own),
            };
            let doc = docs::first_paragraph(&visit.attached, body, self.text);
            self.build
                .define(scope, visit.start..node.end_byte(), what, doc)?;
        }
        if item == Item::Module && body.is_none() {
            self.add_module_file(scope, context.role, visit)?;
        }
        Ok(Context {
            scope: Some(scope),
            within: inner_within,
            role: context.role,
        })
    }

    /// Add `scope`, of a `mod name;` that `visit` declares with `role`,
    /// as a module whose items are in a file of its own: the file a
    /// `#[path = ".."]` names, if one does.
    fn add_module_file(
        &mut self,
        scope: ScopeId,
        role: Role,
        visit: &Visit<'_>,
    ) -> Result<(), IndexFault> {
        let path = visit
            .marks
            .path
            .map(|literal| marks::string_value(literal, self.text));
        self.build.add_module_file(ModuleFile {
            scope,
            role,
            path: path.map(str::to_owned),
        })
    }

    /// Define a `macro_rules!` macro. A macro is private to its module and
    /// those below unless an attribute exports it.
    fn define_macro(&mut self, visit: &Visit<'_>) -> Result<(), IndexFault> {
        let Some(name) = visit.node.child_by_field_name("name") else {
            return Ok(());
        };
        let name = vec![Segment::of_rust(text_of(name, self.text))];
        let scope = self.build.scopes.enter(visit.context.scope, name)?;
        let what = What {
            kind: DefinitionKind::Macro,
            role: visit.context.role,
            visibility: Some(match visit.marks.macro_export {
                true => Visibility::Public,
                false => Visibility::Private,
            }),
        };
        let doc = docs::first_paragraph(&visit.attached, None, self.text);
        self.build
            .define(scope, visit.start..visit.node.end_byte(), what, doc)
    }

    /// An `impl`'s context: the type's segment, then the trait's for a
    /// trait impl, which is itself a definition.
    fn enter_impl(&mut self, visit: &Visit<'_>) -> Result<Context, IndexFault> {
        let Visit {
            node,
            context,
            start,
            ref attached,
            ..
        } = *visit;
        let Some(self_type) = node.child_by_field_name("type") else {
            return Ok(context);
        };
        let type_segment = type_segment(self_type, &self.build);
        Ok(match node.child_by_field_name("trait") {
            Some(trait_node) => {
                let trait_segment =
                    Segment::of_trait(&self.build.spelled(&trait_node.byte_range()));
                let scope = self
                    .build
                    .scopes
                    .enter(context.scope, vec![type_segment, trait_segment])?;
                let what = What {
                    kind: DefinitionKind::Impl,
                    role: context.role,
                    visibility: None,
                };
                let body = node.child_by_field_name("body");
                let doc = docs::first_paragraph(attached, body, self.text);
                self.build
                    .define(scope, start..node.end_byte(), what, doc)?;
                Context {
                    scope: Some(scope),
                    within: Within::TraitImpl,
                    role: context.role,
                }
            }
            None => Context {
                scope: Some(self.build.scopes.enter(context.scope, vec![type_segment])?),
                within: Within::Impl,
                role: context.role,
            },
        })
    }
}

/// The attributes and outer doc comments right before a child: where the
/// first starts, what the attributes say, and the nodes of both.
struct Leading<'n> {
    start: usize,
    marks: Marks<'n>,
    attached: Vec<Node<'n>>,
}

impl Leading<'_> {
    /// The run that starts at byte `start` of a node whose inner
    /// attributes give its children `role`.
    const fn starting_at(start: usize, role: Role) -> Self {
        Self {
            start,
            marks: Marks {
                role,
                macro_export: false,
                path: None,
            },
            attached: Vec::new(),
        }
    }
}

/// Push the named children of `node` to visit in `context`, each with
/// where it starts: at the first of the attributes and outer doc comments
/// right before it; each with the role its attributes give it, and an
/// inner `#![cfg(test)]` gives every child; and each with those nodes.
fn push_children<'n>(stack: &mut Vec<Visit<'n>>, node: Node<'n>, context: Context, text: &str) {
    let inner_role = marks::inner_role(node, context.role, text);
    let mut cursor = node.walk();
    let children: Vec<Node<'n>> = node.named_children(&mut cursor).collect();
    let mut leading: Option<Leading<'n>> = None;
    for child in children {
        let attribute = child.kind() == "attribute_item";
        let comment = matches!(child.kind(), "line_comment" | "block_comment");
        let doc = comment && child.child_by_field_name("outer").is_some();
        // A plain comment between an attribute and its item is neither:
        // the attributes before it still mark the item after it.
        if comment && !doc && leading.is_some() {
            continue;
        }
        let mut run = leading
            .take()
            .unwrap_or_else(|| Leading::starting_at(child.start_byte(), inner_role));
        if attribute {
            run.marks = run.marks.and(marks::attribute_item_marks(child, text));
        }
        let (start, marks) = (run.start, run.marks);
        let attached = match attribute || doc {
            true => {
                run.attached.push(child);
                leading = Some(run);
                Vec::new()
            }
            false => run.attached,
        };
        stack.push(Visit {
            node: child,
            context: Context {
                role: marks.role,
                within: child_within(node, child, context.within),
                ..context
            },
            start,
            marks,
            attached,
        });
    }
}

/// The container `child` of `node` stands in, `within` that of `node`'s
/// children: a variant's discriminant is an expression, so an item in it
/// is visible as its own modifier says, not as the enum.
fn child_within(node: Node<'_>, child: Node<'_>, within: Within) -> Within {
    match node.kind() == "enum_variant" && !child.kind().ends_with("field_declaration_list") {
        true => Within::Items,
        false => within,
    }
}

/// The segment an `impl` gives its type: the last name of its path, its
/// generics and references dropped; a type with no name (a tuple, an
/// array) whole. It is spelled from the tokens its hash holds, so no
/// comment and no trailing comma rustfmt adds is part of it.
fn type_segment(mut node: Node<'_>, build: &TreeIndex<'_>) -> Segment {
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
    Segment::of_rust(&build.spelled(&node.byte_range()))
}
