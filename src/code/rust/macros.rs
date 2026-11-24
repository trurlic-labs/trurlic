//! The lexical rule for macros: what a `macro_rules!` definition and an
//! invocation hold that lets an invocation define a type, read from their
//! token trees, which tree-sitter leaves unparsed. The expansion, which
//! needs the macros of the whole crate, is `expand.rs`'s.
//!
//! A macro can define a type when one of its arms opens, after an optional
//! `$(#[$meta:meta])*`, with literal words and then a `$name:ident` its
//! body defines a type of (`struct $name`): an invocation that opens with
//! those words and a name defines that type, and the type holds each item
//! the macro's body names (`fn new`).

use tree_sitter::Node;

use crate::code::index::{TypeArm, TypeMacro};
use crate::code::kinds::{Container, DefinitionKind, Role, Visibility};
use crate::code::syntax::text_of;

use super::marks;

/// A token an item spelled in an invocation may open with before its
/// keyword: a visibility, a qualifier, an ABI's `extern`.
const ITEM_PREFIXES: [&str; 6] = ["pub", "async", "unsafe", "const", "extern", "default"];

/// The literal words an arm may open with; an arm with more defines
/// nothing.
const MAX_ARM_WORDS: usize = 8;

/// The tokens an invocation keeps: [`MAX_ARM_WORDS`] words, the name, and
/// room for the arguments a foreign macro's name is among.
pub(crate) const MAX_INVOCATION_TOKENS: usize = 16;

/// A keyword whose next token in a macro body is a type's name.
const TYPE_KEYWORDS: [&str; 5] = ["struct", "enum", "union", "trait", "type"];

/// An item an invocation spells in its tokens: its name and the keyword
/// before it, its bytes from its attributes to its body, and what its
/// attributes and visibility state.
pub(super) struct SpelledItem<'t> {
    pub(super) name: &'t str,
    pub(super) keyword: &'t str,
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) visibility: Visibility,
    pub(super) role: Role,
}

/// The type-defining facts of `definition`, a `macro_definition`; `None`
/// when no arm of it can define a type.
pub(super) fn type_macro(definition: Node<'_>, text: &str, exported: bool) -> Option<TypeMacro> {
    let name = text_of(definition.child_by_field_name("name")?, text).to_owned();
    let mut cursor = definition.walk();
    let rules: Vec<_> = definition
        .named_children(&mut cursor)
        .filter(|rule| rule.kind() == "macro_rule")
        .collect();
    let bodies: Vec<Vec<Node<'_>>> = rules
        .iter()
        .map(|rule| {
            rule.child_by_field_name("right")
                .map(|body| tokens_of(body, usize::MAX))
                .unwrap_or_default()
        })
        .collect();
    // An arm defines the type its own body does, or one its body has
    // another arm define (`m!(@inner $name)`).
    let arms: Vec<TypeArm> = rules
        .iter()
        .zip(&bodies)
        .filter_map(|(rule, own)| {
            let (words, variable) = arm_opening(rule.child_by_field_name("left")?, text)?;
            let kind = type_kind_of(std::slice::from_ref(own), &variable, text)
                .or_else(|| type_kind_of(&bodies, &variable, text))?;
            Some(TypeArm { words, kind })
        })
        .collect();
    if arms.is_empty() {
        return None;
    }
    Some(TypeMacro {
        name,
        exported,
        arms,
        members: members_of(&bodies, text),
    })
}

/// The names the macro's bodies give items, each a member of the type it
/// defines, with the kind its keyword opens as one; sorted.
fn members_of(bodies: &[Vec<Node<'_>>], text: &str) -> Vec<(String, DefinitionKind)> {
    let mut members: Vec<(String, DefinitionKind)> = bodies
        .iter()
        .flat_map(|tokens| tokens.windows(2))
        .filter_map(|pair| match pair {
            [keyword, name] if name.kind() == "identifier" => {
                DefinitionKind::of_keyword(text_of(*keyword, text), Container::Type)
                    .map(|kind| (text_of(*name, text).to_owned(), kind))
            }
            _ => None,
        })
        .collect();
    members.sort_unstable();
    members.dedup();
    members
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
            tokens_of(*child, room)
                .into_iter()
                .map(|token| text_of(token, text).to_owned()),
        );
        if tokens.len() == MAX_INVOCATION_TOKENS {
            break;
        }
    }
    Some((text_of(name, text).to_owned(), tokens))
}

/// The items `invocation` spells, at any depth of its token trees: an
/// item keyword followed by a name, each with its span: from its
/// attributes, visibility and qualifiers to the first braced tree or `;`
/// after its name, or to its name when neither follows.
pub(super) fn spelled_items<'t>(invocation: Node<'_>, text: &'t str) -> Vec<SpelledItem<'t>> {
    let mut found = Vec::new();
    let mut cursor = invocation.walk();
    let mut trees: Vec<Node<'_>> = invocation
        .named_children(&mut cursor)
        .filter(|child| child.kind() == "token_tree")
        .collect();
    while let Some(tree) = trees.pop() {
        let tokens = inner_children(tree);
        let ends = item_ends(&tokens, text);
        for (at, pair) in tokens.windows(2).enumerate() {
            let [keyword, name] = pair else {
                continue;
            };
            let keyword = text_of(*keyword, text);
            let is_item = DefinitionKind::of_keyword(keyword, Container::Items).is_some();
            if name.kind() != "identifier" || !is_item {
                continue;
            }
            let first = spelled_item_start(&tokens, at, text);
            let prefix = tokens.get(first..at).unwrap_or_default();
            let end = ends
                .get(at + 2)
                .copied()
                .flatten()
                .unwrap_or(name.end_byte());
            found.push(SpelledItem {
                name: text_of(*name, text),
                keyword,
                start: tokens.get(first).map_or(0, Node::start_byte),
                end,
                visibility: prefix_visibility(prefix, text),
                role: prefix_role(prefix, text),
            });
        }
        trees.extend(tokens.iter().filter(|token| token.kind() == "token_tree"));
    }
    found
}

/// For each of `tokens`, where the first braced tree or `;` from it on
/// ends: one pass from the back, so a list of keywords costs no rescan.
fn item_ends(tokens: &[Node<'_>], text: &str) -> Vec<Option<usize>> {
    let mut ends = vec![None; tokens.len() + 1];
    for (at, token) in tokens.iter().enumerate().rev() {
        let spelled = text_of(*token, text);
        let own = (spelled == ";" || spelled.starts_with('{')).then(|| token.end_byte());
        let next = ends.get(at + 1).copied().flatten();
        if let Some(end) = ends.get_mut(at) {
            *end = own.or(next);
        }
    }
    ends
}

/// The visibility the tokens before an item's keyword state: a `pub` and
/// the group after it.
fn prefix_visibility(prefix: &[Node<'_>], text: &str) -> Visibility {
    let spelled: Vec<&str> = prefix.iter().map(|token| text_of(*token, text)).collect();
    let Some(at) = spelled.iter().position(|token| *token == "pub") else {
        return Visibility::Private;
    };
    let group = spelled
        .get(at + 1)
        .filter(|group| group.starts_with('('))
        .unwrap_or(&"");
    Visibility::of_modifier(&format!("pub{group}")).unwrap_or(Visibility::Public)
}

/// Test code when an attribute before an item's keyword makes it.
fn prefix_role(prefix: &[Node<'_>], text: &str) -> Role {
    prefix
        .windows(2)
        .filter(|pair| matches!(pair, [hash, _] if text_of(*hash, text) == "#"))
        .filter_map(|pair| pair.get(1))
        .map(|group| {
            let tokens: Vec<&str> = tokens_of(*group, usize::MAX)
                .into_iter()
                .map(|token| text_of(token, text))
                .collect();
            // The group's tokens between its brackets.
            let inside = tokens
                .get(1..tokens.len().saturating_sub(1))
                .unwrap_or_default();
            marks::attribute_role(inside)
        })
        .fold(Role::Code, Role::or)
}

/// The index of the first token of the item whose keyword is
/// `tokens[at]`: its attributes, visibility and qualifiers.
fn spelled_item_start(tokens: &[Node<'_>], at: usize, text: &str) -> usize {
    let mut start = at;
    loop {
        let before = |back: usize| start.checked_sub(back).and_then(|n| tokens.get(n));
        let token = |back: usize| before(back).map_or("", |node| text_of(*node, text));
        start -= match (token(2), token(1)) {
            (_, word) if ITEM_PREFIXES.contains(&word) => 1,
            (_, literal) if literal.starts_with('"') && token(2) == "extern" => 1,
            ("pub", group) if group.starts_with('(') => 2,
            ("#", group) if group.starts_with('[') => 2,
            _ => break,
        };
    }
    start
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

/// The kind of the type a body of the macro defines of `variable`, by the
/// first type keyword followed by it; `None` when none is.
fn type_kind_of(bodies: &[Vec<Node<'_>>], variable: &str, text: &str) -> Option<DefinitionKind> {
    bodies
        .iter()
        .flat_map(|tokens| tokens.windows(2))
        .find_map(|pair| match pair {
            [keyword, name]
                if TYPE_KEYWORDS.contains(&text_of(*keyword, text))
                    && name.kind() == "metavariable"
                    && text_of(*name, text) == variable =>
            {
                DefinitionKind::of_keyword(text_of(*keyword, text), Container::Items)
            }
            _ => None,
        })
}

/// The children of a token tree or pattern between its delimiters,
/// comments left out.
fn inner_children(tree: Node<'_>) -> Vec<Node<'_>> {
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

/// The tokens of `node`, in order, at most `limit`: its leaves, a literal
/// as one token, comments left out.
pub(super) fn tokens_of(node: Node<'_>, limit: usize) -> Vec<Node<'_>> {
    let mut tokens = Vec::new();
    let mut cursor = node.walk();
    'walk: while tokens.len() < limit {
        let current = cursor.node();
        let unit = current.child_count() == 0 || current.kind().ends_with("_literal");
        if !current.is_extra() {
            if unit {
                if current.end_byte() > current.start_byte() {
                    tokens.push(current);
                }
            } else if cursor.goto_first_child() {
                continue;
            }
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() || cursor.node() == node {
                break 'walk;
            }
        }
    }
    tokens
}

/// A run of letters, digits and underscores: an identifier or a keyword.
pub(crate) fn is_word(token: &str) -> bool {
    !token.is_empty() && token.chars().all(|c| c.is_alphanumeric() || c == '_')
}
