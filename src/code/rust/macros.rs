//! The lexical rule for macros: what a `macro_rules!` definition and an
//! invocation hold that lets an invocation define a type, read from their
//! token trees, which tree-sitter leaves unparsed. The expansion, which
//! needs the macros of the whole crate, is `expand.rs`'s.
//!
//! A macro can define a type when one of its arms opens, after an optional
//! `$(#[$meta:meta])*`, with literal words and then a `$name:ident` its
//! body defines a type of (`struct $name`): an invocation that opens with
//! those words and a name defines that type. The type holds the items of
//! each `impl` in a body whose self type is a metavariable an arm binds
//! as an `ident` (`impl $name { fn new() }`, `impl Display for $name`),
//! a trait impl's under the trait's segment; an `impl` of any other type
//! (`impl From<$name> for String`) gives the type nothing.

use std::collections::BTreeSet;
use std::ops::ControlFlow;

use tree_sitter::Node;

use crate::code::index::{IndexFault, Member, TypeMacro};
use crate::code::kinds::{Container, DefinitionKind};
use crate::code::syntax::{self, Token, text_of};
use crate::code::work::Work;

use super::{LITERALS, arms};

/// The tokens an invocation keeps: [`arms::MAX_ARM_WORDS`] words, the name, and
/// room for the arguments a foreign macro's name is among.
const MAX_INVOCATION_TOKENS: usize = 16;

/// Names a macro's body gives items, each with its kind.
type Named = Vec<(String, DefinitionKind)>;

/// The type-defining facts of `definition`, a `macro_definition`; `None`
/// when it neither has an arm that defines a type nor defines a name it
/// is handed (`arms.rs`). What else the bodies define is the macro's, so
/// the arm a recursive call reaches gives its items (`body_items`). Each
/// body's tokens are read a few times at most and spent from `work`.
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
    let trees: Vec<Node<'_>> = rules
        .iter()
        .filter_map(|rule| rule.child_by_field_name("right"))
        .collect();
    let names = arms::ident_variables(&rules, text, work)?;
    let defined = arms::defined(&rules, &names, text, work)?;
    if defined.arms.is_empty() && defined.defines.is_none() {
        return Ok(None);
    }
    let (members, items) = body_items(&trees, &names, text, work)?;
    Ok(Some(TypeMacro {
        name: text_of(name, text).to_owned(),
        exported,
        arms: defined.arms,
        defines: defined.defines,
        members,
        items,
    }))
}

/// What the macro's bodies define with a name they spell: each item an
/// `impl` of a metavariable of `names` holds, a member of the type, with
/// the kind its keyword opens as one; and each other item at a body's top
/// level (`pub fn make()`), an item of the invocation's container. Both
/// sorted.
fn body_items(
    bodies: &[Node<'_>],
    names: &BTreeSet<&str>,
    text: &str,
    work: &mut Work,
) -> Result<(Vec<Member>, Named), IndexFault> {
    let mut members = Vec::new();
    let mut items = Vec::new();
    for body in bodies {
        let top = inner_children(*body);
        work.spend(top.len())?;
        let mut at = 0;
        while let Some(token) = top.get(at) {
            at += 1;
            let keyword = text_of(*token, text);
            if keyword == "impl" {
                let Some(open) = top
                    .iter()
                    .skip(at)
                    .position(|t| text_of(*t, text).starts_with('{'))
                else {
                    break;
                };
                let header = top.get(at..at + open).unwrap_or_default();
                if let Some(block) = top.get(at + open) {
                    members.extend(impl_members(header, *block, names, text, work)?);
                }
                at += open + 1;
                continue;
            }
            let pair = [*token, top.get(at).copied().unwrap_or(*token)];
            items.extend(named_items(&pair, Container::Items, text));
        }
    }
    members.sort_unstable();
    members.dedup();
    items.sort_unstable();
    items.dedup();
    Ok((members, items))
}

/// The members of the type an `impl` with `header` gives in its `block`:
/// none unless its self type is a metavariable of `names`.
fn impl_members(
    header: &[Node<'_>],
    block: Node<'_>,
    names: &BTreeSet<&str>,
    text: &str,
    work: &mut Work,
) -> Result<Vec<Member>, IndexFault> {
    let Some(trait_tokens) = impl_of_name(header, names, text) else {
        return Ok(Vec::new());
    };
    let inside = inner_children(block);
    work.spend(inside.len())?;
    Ok(named_items(&inside, Container::Type, text)
        .into_iter()
        .map(|(name, kind)| Member {
            // clone: each member of a trait impl names its trait.
            trait_tokens: trait_tokens.clone(),
            name,
            kind,
        })
        .collect())
}

/// For an `impl` whose `header` (its tokens between `impl` and its block)
/// names as its self type one metavariable of `names`: `Some(None)` for
/// an inherent impl, `Some(Some(trait))` for a trait impl, the trait's
/// tokens with each metavariable of `names` written `$`. `None` for an
/// impl of any other type.
fn impl_of_name(
    header: &[Node<'_>],
    names: &BTreeSet<&str>,
    text: &str,
) -> Option<Option<Vec<String>>> {
    let spelled = |node: &Node<'_>| text_of(*node, text);
    // Generics after `impl` first: their angle brackets balanced.
    let mut depth = 0_usize;
    let mut rest = header;
    if rest.first().is_some_and(|first| spelled(first) == "<") {
        let close = rest.iter().position(|token| {
            let token = spelled(token);
            depth += token.matches('<').count();
            depth = depth.saturating_sub(token.matches('>').count());
            depth == 0
        })?;
        rest = rest.get(close + 1..)?;
    }
    let before_where = rest
        .iter()
        .position(|token| spelled(token) == "where")
        .map_or(rest, |at| rest.get(..at).unwrap_or_default());
    let (trait_tokens, self_type) = match before_where.iter().position(|t| spelled(t) == "for") {
        Some(at) => (Some(before_where.get(..at)?), before_where.get(at + 1..)?),
        None => (None, before_where),
    };
    match self_type {
        [only] if only.kind() == "metavariable" && names.contains(spelled(only)) => {}
        _ => return None,
    }
    Some(trait_tokens.map(|tokens| {
        tokens
            .iter()
            .map(|token| match names.contains(spelled(token)) {
                true => "$".to_owned(),
                false => spelled(token).to_owned(),
            })
            .collect()
    }))
}

/// Each item keyword of `tokens` followed by an identifier: its name, and
/// the kind it opens in `container`.
fn named_items(
    tokens: &[Node<'_>],
    container: Container,
    text: &str,
) -> Vec<(String, DefinitionKind)> {
    tokens
        .windows(2)
        .filter_map(|pair| match pair {
            [keyword, name] if name.kind() == "identifier" => {
                DefinitionKind::of_keyword(text_of(*keyword, text), container)
                    .map(|kind| (text_of(*name, text).to_owned(), kind))
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
    syntax::tokens(node, text, &LITERALS, |token| {
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
