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

use std::collections::{BTreeMap, BTreeSet};
use std::ops::ControlFlow;

use tree_sitter::Node;

use crate::code::index::{IndexFault, Member, TypeArm, TypeMacro};
use crate::code::kinds::{Container, DefinitionKind};
use crate::code::syntax::{self, Token, text_of};
use crate::code::work::Work;

use super::LITERALS;

/// The literal words an arm may open with; an arm with more defines
/// nothing.
pub(crate) const MAX_ARM_WORDS: usize = 8;

/// The tokens an invocation keeps: [`MAX_ARM_WORDS`] words, the name, and
/// room for the arguments a foreign macro's name is among.
const MAX_INVOCATION_TOKENS: usize = 16;

/// Names a macro's body gives items, each with its kind.
type Named = Vec<(String, DefinitionKind)>;

/// A keyword whose next token in a macro body is a type's name.
const TYPE_KEYWORDS: [&str; 5] = ["struct", "enum", "union", "trait", "type"];

/// The type-defining facts of `definition`, a `macro_definition`; `None`
/// when no arm of it can define a type. An arm's type and its kind are
/// the arm's own: its body's type keyword before its `$name`, or, when its
/// body hands the name to another arm (`m!(@methods $name)`), the first
/// such keyword of any body. What else the bodies define is the macro's,
/// so the arm a recursive call reaches gives its items (`body_items`).
/// Each body's tokens are read a few times at most and spent from `work`.
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
    let arms = type_arms(&rules, text, work)?;
    if arms.is_empty() {
        return Ok(None);
    }
    let names = ident_variables(&rules, text, work)?;
    let (members, items) = body_items(&trees, &names, text, work)?;
    Ok(Some(TypeMacro {
        name: text_of(name, text).to_owned(),
        exported,
        arms,
        members,
        items,
    }))
}

/// The arms of `rules` that define a type, each with its words and the
/// type's kind.
fn type_arms(rules: &[Node<'_>], text: &str, work: &mut Work) -> Result<Vec<TypeArm>, IndexFault> {
    let mut bodies: Vec<Vec<Node<'_>>> = Vec::with_capacity(rules.len());
    for rule in rules {
        let body = rule
            .child_by_field_name("right")
            .map(|body| tokens_of(body, text, usize::MAX))
            .unwrap_or_default();
        work.spend(body.len())?;
        bodies.push(body);
    }
    let any_body = type_kinds(&bodies, text);
    let mut arms: Vec<TypeArm> = Vec::new();
    for (rule, own) in rules.iter().zip(&bodies) {
        // Each arm reads its own body once more: twice the macro in all.
        work.spend(own.len())?;
        let Some(pattern) = rule.child_by_field_name("left") else {
            continue;
        };
        let Some((words, variable)) = arm_opening(pattern, text) else {
            continue;
        };
        let own_kinds = type_kinds(std::slice::from_ref(own), text);
        let kind = own_kinds
            .get(variable.as_str())
            .or_else(|| any_body.get(variable.as_str()));
        if let Some(kind) = kind {
            arms.push(TypeArm { words, kind: *kind });
        }
    }
    Ok(arms)
}

/// The metavariables the patterns of `rules` bind as an `ident`
/// (`$name:ident`), at any depth of a repetition.
fn ident_variables<'t>(
    rules: &[Node<'_>],
    text: &'t str,
    work: &mut Work,
) -> Result<BTreeSet<&'t str>, IndexFault> {
    let mut names = BTreeSet::new();
    for pattern in rules
        .iter()
        .filter_map(|rule| rule.child_by_field_name("left"))
    {
        let mut pending = vec![pattern];
        while let Some(node) = pending.pop() {
            work.spend(1)?;
            let ident = node.kind() == "token_binding_pattern"
                && node
                    .child_by_field_name("type")
                    .is_some_and(|fragment| text_of(fragment, text) == "ident");
            match (ident, node.child_by_field_name("name")) {
                (true, Some(variable)) => {
                    names.insert(text_of(variable, text));
                }
                (true, None) | (false, _) => {
                    let mut cursor = node.walk();
                    pending.extend(node.named_children(&mut cursor));
                }
            }
        }
    }
    Ok(names)
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

/// The words an arm's pattern opens with and the `$name` of the `ident`
/// that follows them; `None` for an arm that opens otherwise.
fn arm_opening(pattern: Node<'_>, text: &str) -> Option<(Vec<String>, String)> {
    let inside = inner_children(pattern);
    let mut rest = inside.iter().peekable();
    if rest
        .peek()
        .is_some_and(|first| is_attribute_repetition(**first, text))
    {
        rest.next();
    }
    let mut words = Vec::new();
    for node in rest {
        if node.kind() == "token_binding_pattern" {
            let fragment = node.child_by_field_name("type")?;
            let variable = node.child_by_field_name("name")?;
            return (text_of(fragment, text) == "ident")
                .then(|| (words, text_of(variable, text).to_owned()));
        }
        let word = text_of(*node, text);
        if node.child_count() > 0 || !is_word(word) || words.len() == MAX_ARM_WORDS {
            return None;
        }
        words.push(word.to_owned());
    }
    None
}

/// Whether `node` is `$(#[$meta:meta])*`, the attributes an invocation
/// may open with.
fn is_attribute_repetition(node: Node<'_>, text: &str) -> bool {
    let spelled: String = text_of(node, text)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    node.kind() == "token_repetition_pattern"
        && spelled.starts_with("$(#[$")
        && spelled.ends_with(":meta])*")
}

/// The kind of the type each metavariable names in `bodies`: the first
/// type keyword before it; one pass over the tokens.
fn type_kinds<'t>(bodies: &[Vec<Node<'_>>], text: &'t str) -> BTreeMap<&'t str, DefinitionKind> {
    let mut kinds = BTreeMap::new();
    for pair in bodies.iter().flat_map(|tokens| tokens.windows(2)) {
        if let [keyword, name] = pair
            && name.kind() == "metavariable"
            && TYPE_KEYWORDS.contains(&text_of(*keyword, text))
            && let Some(kind) =
                DefinitionKind::of_keyword(text_of(*keyword, text), Container::Items)
        {
            kinds.entry(text_of(*name, text)).or_insert(kind);
        }
    }
    kinds
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
