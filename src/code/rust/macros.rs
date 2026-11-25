//! The lexical rule for macros: what a `macro_rules!` definition and an
//! invocation hold that lets an invocation define a type, read from their
//! token trees, which tree-sitter leaves unparsed. The expansion, which
//! needs the macros of the whole crate, is `expand.rs`'s.
//!
//! A macro can define a type when one of its arms opens with literal
//! words and then a `$name:ident` (`arms.rs`). The type holds the items of
//! each `impl` whose self type is the name a rule receives, alone or with
//! generic arguments: `impl $name { fn new() }`, `impl<T> $name<T>`,
//! `impl Display for $name`, a trait impl's under the trait's segment. An
//! `impl` of any other type (`impl From<$name> for String`, `impl $other`
//! of a second name) gives the type nothing.

use std::ops::ControlFlow;
use std::sync::Arc;

use tree_sitter::Node;

use crate::code::index::{IndexFault, Receives, TypeImpl, TypeMacro};
use crate::code::kinds::{Container, DefinitionKind};
use crate::code::symbol::Segment;
use crate::code::syntax::{self, Token, text_of};
use crate::code::work::Work;

use super::{LEXICON, arms};

/// The tokens an invocation keeps: [`arms::MAX_ARM_WORDS`] words, the name, and
/// room for the arguments a foreign macro's name is among.
pub(in crate::code) const MAX_INVOCATION_TOKENS: usize = 16;

/// Names a macro's body gives items, each with its kind.
pub(super) type Named = Vec<(Arc<Segment>, DefinitionKind)>;

/// The type-defining facts of `definition`, a `macro_definition`; `None`
/// when no rule of it spells a type of a name it binds, hands one on or
/// holds an `impl` of one (`arms.rs`). What else the bodies define is the macro's, so the rule a
/// recursive call reaches gives its items (`body_items`). Each body's
/// tokens are read a few times at most and spent from `work`.
pub(super) fn type_macro(
    definition: Node<'_>,
    text: &str,
    exported: bool,
    work: &mut Work,
) -> Result<Option<TypeMacro>, IndexFault> {
    let Some(name) = definition.child_by_field_name("name") else {
        return Ok(None);
    };
    let mut cursor = definition.walk();
    let rules: Vec<_> = definition
        .named_children(&mut cursor)
        .filter(|rule| rule.kind() == "macro_rule")
        .collect();
    let (arms, rules_read) = arms::rules_of(&rules, text, work)?;
    let gives_names = rules_read.iter().any(|rule| {
        rule.receives
            .iter()
            .any(|(_, receives)| *receives != Receives::default())
    });
    if !gives_names {
        return Ok(None);
    }
    Ok(Some(TypeMacro {
        name: text_of(name, text).to_owned(),
        exported,
        arms,
        rules: rules_read,
    }))
}

/// What a rule's body holds at its top level: the items it names but
/// types (`pub fn make()`), sorted, and each `impl`, its header (its
/// tokens between `impl` and its block) and its block.
pub(super) struct Body<'n> {
    pub(super) items: Named,
    impls: Vec<(&'n [Node<'n>], Node<'n>)>,
}

/// The top-level tokens of a body, each read once.
pub(super) fn top_of<'n>(body: Node<'n>, work: &mut Work) -> Result<Vec<Node<'n>>, IndexFault> {
    let top = inner_children(body);
    work.spend(top.len())?;
    Ok(top)
}

/// What `top`, a body's top-level tokens, holds.
pub(super) fn body_of<'n>(top: &'n [Node<'n>], text: &str) -> Body<'n> {
    let mut items = Vec::new();
    let mut impls = Vec::new();
    let mut at = 0;
    while let Some(token) = top.get(at) {
        at += 1;
        if text_of(*token, text) != "impl" {
            let pair = [*token, top.get(at).copied().unwrap_or(*token)];
            items.extend(named_items(&pair, Container::Items, text));
            continue;
        }
        let Some(open) = top
            .iter()
            .skip(at)
            .position(|t| text_of(*t, text).starts_with('{'))
        else {
            break;
        };
        if let (Some(header), Some(block)) = (top.get(at..at + open), top.get(at + open)) {
            impls.push((header, *block));
        }
        at += open + 1;
    }
    items.sort_unstable();
    items.dedup();
    Body { items, impls }
}

/// The `impl`s of `body` of the type the metavariable `name` names,
/// whose items are its members, with the kind their keyword opens as one.
pub(super) fn impls_of(
    body: &Body<'_>,
    name: &str,
    text: &str,
    work: &mut Work,
) -> Result<Vec<TypeImpl>, IndexFault> {
    let mut found = Vec::new();
    for (header, block) in &body.impls {
        found.extend(type_impl(header, *block, name, text, work)?);
    }
    found.sort_unstable();
    found.dedup();
    Ok(found)
}

/// The `impl` of the type `name` names with `header` and `block`; `None`
/// for an impl of any other type.
fn type_impl(
    header: &[Node<'_>],
    block: Node<'_>,
    name: &str,
    text: &str,
    work: &mut Work,
) -> Result<Option<TypeImpl>, IndexFault> {
    let Some(trait_tokens) = impl_of_name(header, name, text) else {
        return Ok(None);
    };
    let inside = inner_children(block);
    work.spend(inside.len())?;
    Ok(Some(TypeImpl {
        trait_tokens,
        members: named_items(&inside, Container::Type, text),
    }))
}

/// What an `impl` whose `header` (its tokens between `impl` and its
/// block) gives the metavariable `name`; `None` when its self type is
/// another than `name`, alone or with generic arguments (`$name<T>`).
/// `Some(None)` is an inherent impl, and `Some(Some(trait))` a trait impl,
/// the trait's tokens with `name` written `$`.
fn impl_of_name(header: &[Node<'_>], name: &str, text: &str) -> Option<Option<Vec<String>>> {
    let spelled = |node: &Node<'_>| text_of(*node, text);
    let rest = match header.first().is_some_and(|first| spelled(first) == "<") {
        true => header.get(angle_close(header, text)? + 1..)?,
        false => header,
    };
    let before_where = rest
        .iter()
        .position(|token| spelled(token) == "where")
        .map_or(rest, |at| rest.get(..at).unwrap_or_default());
    let (trait_tokens, self_type) = match before_where.iter().position(|t| spelled(t) == "for") {
        Some(at) => (Some(before_where.get(..at)?), before_where.get(at + 1..)?),
        None => (None, before_where),
    };
    let (first, arguments) = self_type.split_first()?;
    let generic = arguments.is_empty()
        || (arguments.first().is_some_and(|open| spelled(open) == "<")
            && angle_close(arguments, text) == Some(arguments.len() - 1));
    if first.kind() != "metavariable" || spelled(first) != name || !generic {
        return None;
    }
    Some(trait_tokens.map(|tokens| {
        tokens
            .iter()
            .map(|token| match spelled(token) == name {
                true => "$".to_owned(),
                false => spelled(token).to_owned(),
            })
            .collect()
    }))
}

/// The index of the token that closes the angle bracket `tokens` opens
/// with: `<` and `<<` open, `>` and `>>` close, and no other token counts
/// (`->`, `=>`, `>=` are no brackets).
fn angle_close(tokens: &[Node<'_>], text: &str) -> Option<usize> {
    let mut depth = 0_usize;
    tokens.iter().position(|token| {
        match text_of(*token, text) {
            "<" => depth += 1,
            "<<" => depth += 2,
            ">" => depth = depth.saturating_sub(1),
            ">>" => depth = depth.saturating_sub(2),
            _ => {}
        }
        depth == 0
    })
}

/// Each item keyword of `tokens` followed by an identifier: its name, and
/// the kind it opens in `container`.
fn named_items(tokens: &[Node<'_>], container: Container, text: &str) -> Named {
    tokens
        .windows(2)
        .filter_map(|pair| match pair {
            [keyword, name] if name.kind() == "identifier" => {
                DefinitionKind::of_keyword(text_of(*keyword, text), container)
                    .map(|kind| (Arc::new(Segment::name(text_of(*name, text))), kind))
            }
            _ => None,
        })
        .collect()
}

/// The macro's name and the first tokens of `invocation`, a
/// `macro_invocation`, its attributes and comments left out.
pub(super) fn invocation_tokens(invocation: Node<'_>, text: &str) -> Option<(String, Vec<String>)> {
    let path = invocation.child_by_field_name("macro")?;
    let name = path.child_by_field_name("name").unwrap_or(path);
    let mut cursor = invocation.walk();
    let tree = invocation
        .named_children(&mut cursor)
        .find(|child| child.kind() == "token_tree")?;
    let inside = inner_children(tree);
    let mut tokens = Vec::new();
    for child in skip_attributes(&inside, text) {
        let room = MAX_INVOCATION_TOKENS - tokens.len();
        tokens.extend(
            tokens_of(*child, text, room)
                .into_iter()
                .map(|token| text_of(token, text).to_owned()),
        );
        if tokens.len() == MAX_INVOCATION_TOKENS {
            break;
        }
    }
    Some((text_of(name, text).to_owned(), tokens))
}

/// The children of a token tree or pattern between its delimiters,
/// comments left out.
pub(super) fn inner_children(tree: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = tree.walk();
    let children: Vec<_> = tree
        .children(&mut cursor)
        .filter(|c| !c.is_extra())
        .collect();
    match children.as_slice() {
        [_, inside @ .., _] => inside.to_vec(),
        [] | [_] => Vec::new(),
    }
}

/// `children` past the attributes they open with: `#` or `#!`, then a
/// bracketed tree.
fn skip_attributes<'a, 'n>(children: &'a [Node<'n>], text: &str) -> &'a [Node<'n>] {
    let mut rest = children;
    loop {
        rest = match rest {
            [hash, bang, tree, after @ ..]
                if text_of(*hash, text) == "#"
                    && text_of(*bang, text) == "!"
                    && text_of(*tree, text).starts_with('[') =>
            {
                after
            }
            [hash, tree, after @ ..]
                if text_of(*hash, text) == "#" && text_of(*tree, text).starts_with('[') =>
            {
                after
            }
            _ => return rest,
        };
    }
}

/// The nodes of the tokens of `node`, in order, at most `limit`: its
/// leaves, a literal as one token, comments left out (`syntax::tokens`).
pub(super) fn tokens_of<'n>(node: Node<'n>, text: &str, limit: usize) -> Vec<Node<'n>> {
    let mut found = Vec::new();
    syntax::tokens(node, text, &LEXICON, |token| {
        if let Token::Node(node) = token {
            found.push(node);
        }
        match found.len() < limit {
            true => ControlFlow::Continue(()),
            false => ControlFlow::Break(()),
        }
    });
    found
}

/// A run of letters, digits and underscores: an identifier or a keyword.
pub(crate) fn is_word(token: &str) -> bool {
    !token.is_empty() && token.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// Rust's strict and reserved keywords: no item and no `$name:ident` a
/// type is defined by is named one.
const KEYWORDS: [&str; 39] = [
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type",
    "unsafe", "use", "where", "while", "abstract",
];

/// Whether `token` can name an item: a word, no keyword.
pub(crate) fn is_name(token: &str) -> bool {
    is_word(token) && !KEYWORDS.contains(&token)
}
