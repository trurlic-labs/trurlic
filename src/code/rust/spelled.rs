//! The lexical rule for an item a macro invocation spells in its tokens,
//! which tree-sitter leaves unparsed. It is an item keyword followed by a
//! name (`define! { pub struct View { .. } }`, `proptest! { fn case() {}
//! }`), with the named fields and variants a spelled `struct`, `union` or
//! `enum` holds in its braces. The items of a spelled `mod`'s braces are the
//! module's. A tree that is another item's body, or that follows a name
//! (`class!(Meters { fn new() })`, whose items the macro may make any
//! type's members), defines nothing: a wrong path is worse than none. Any
//! other tree is read as the invocation's own (`cfg_if! { if .. { .. } }`).
//!
//! A spelled item spans from its attributes, visibility and qualifiers to
//! the first braced tree or `;` after its name. It spans to its name alone
//! when the next item of its tree comes first, so a run of keywords spans
//! each item alone. Every token read is spent from the file's work budget.

use std::collections::BTreeMap;

use tree_sitter::Node;

use crate::code::index::IndexFault;
use crate::code::kinds::{Container, DefinitionKind, Role, Visibility};
use crate::code::syntax::text_of;
use crate::code::work::Work;

use super::macros::{inner_children, is_name, tokens_of};
use super::marks;

/// A token an item spelled in an invocation may open with before its
/// keyword: a visibility, a qualifier, an ABI's `extern`.
const ITEM_PREFIXES: [&str; 6] = ["pub", "async", "unsafe", "const", "extern", "default"];

/// What a spelled item, or a field or variant of one, is: its name, its
/// bytes from its attributes on, its visibility and role as its own
/// tokens state them.
pub(super) struct Spelled<'t> {
    pub(super) name: &'t str,
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) visibility: Visibility,
    pub(super) role: Role,
}

/// An item an invocation spells, with the keyword before its name, the
/// members its braces hold, and the spelled module it is in, by its place
/// among the items found before it.
pub(super) struct SpelledItem<'t> {
    pub(super) item: Spelled<'t>,
    pub(super) keyword: &'t str,
    pub(super) members: Vec<Member<'t>>,
    pub(super) within: Option<usize>,
    /// The place of its body among the tokens of its tree.
    body: Option<usize>,
}

/// A named field of a spelled struct or union, or a variant of a spelled
/// enum with the named fields of its braces.
pub(super) struct Member<'t> {
    pub(super) member: Spelled<'t>,
    pub(super) kind: DefinitionKind,
    pub(super) fields: Vec<Spelled<'t>>,
}

/// The items `invocation` spells, in its own trees and in its spelled
/// modules' bodies.
pub(super) fn spelled_items<'t>(
    invocation: Node<'_>,
    text: &'t str,
    work: &mut Work,
) -> Result<Vec<SpelledItem<'t>>, IndexFault> {
    let mut found: Vec<SpelledItem<'t>> = Vec::new();
    let mut cursor = invocation.walk();
    // Each tree with the role of what holds it (a test module's body, or a
    // tree an attribute marks: `cfg_if! { if #[cfg(test)] { .. } }`) and
    // the spelled module it is the body of or in.
    let mut trees: Vec<(Node<'_>, Role, Option<usize>)> = invocation
        .named_children(&mut cursor)
        .filter(|child| child.kind() == "token_tree")
        .map(|tree| (tree, Role::Code, None))
        .collect();
    while let Some((tree, role, within)) = trees.pop() {
        let tokens = inner_children(tree);
        work.spend(tokens.len())?;
        let mut roles = vec![role; tokens.len()];
        let first = found.len();
        found.extend(
            read_tree(&tokens, role, &mut roles, text, work)?
                .into_iter()
                .map(|mut item| {
                    item.within = within;
                    item
                }),
        );
        mark_attributed_trees(&tokens, &mut roles, text);
        // The place of each item's body among `tokens`, and the module the
        // body is of: the item itself when it is a `mod`.
        let bodies: BTreeMap<usize, Option<usize>> = (first..found.len())
            .filter_map(|at| {
                let item = found.get(at)?;
                Some((item.body?, (item.keyword == "mod").then_some(at)))
            })
            .collect();
        trees.extend(nested_trees(&tokens, roles, &bodies, within, text));
    }
    Ok(found)
}

/// The trees among `tokens`, each with its role, to read for items. A
/// spelled module's body is read in that module; a tree that is no
/// item's body and follows no name is read in `within`, the module of the
/// tree `tokens` are of. `bodies` holds each item body's place and its
/// module, when the item is a `mod`.
fn nested_trees<'n>(
    tokens: &[Node<'n>],
    roles: Vec<Role>,
    bodies: &BTreeMap<usize, Option<usize>>,
    within: Option<usize>,
    text: &str,
) -> Vec<(Node<'n>, Role, Option<usize>)> {
    let mut found = Vec::new();
    for (at, (token, role)) in tokens.iter().zip(roles).enumerate() {
        if token.kind() != "token_tree" {
            continue;
        }
        let body = bodies.get(&at);
        let after_a_name = at
            .checked_sub(1)
            .and_then(|before| tokens.get(before))
            .is_some_and(|before| before.kind() == "identifier" && is_name(text_of(*before, text)));
        match body {
            Some(Some(module)) => found.push((*token, role, Some(*module))),
            Some(None) => {}
            None if after_a_name => {}
            None => found.push((*token, role, within)),
        }
    }
    found
}

/// The items the `tokens` of one tree of `role` spell; each item's body
/// gets the item's role in `roles`.
fn read_tree<'t>(
    tokens: &[Node<'_>],
    role: Role,
    roles: &mut [Role],
    text: &'t str,
    work: &mut Work,
) -> Result<Vec<SpelledItem<'t>>, IndexFault> {
    let mut found = Vec::new();
    let ends = item_ends(tokens, text);
    for (at, pair) in tokens.windows(2).enumerate() {
        let [keyword, name] = pair else {
            continue;
        };
        let keyword = text_of(*keyword, text);
        // `enum a::B` names a path, and `enum B as C` renames one: no item
        // is either in Rust.
        let path = tokens
            .get(at + 2)
            .is_some_and(|next| matches!(text_of(*next, text), "::" | "as"));
        if !starts_item(keyword, *name, text) || path {
            continue;
        }
        let first = spelled_item_start(tokens, at, text);
        let prefix = tokens.get(first..at).unwrap_or_default();
        let end = ends.get(at + 2).copied().flatten();
        let body = end.and_then(|end| tokens.get(end));
        let members = match body.filter(|body| text_of(**body, text).starts_with('{')) {
            Some(body) => members(*body, keyword, text, work)?,
            None => Vec::new(),
        };
        let item_role = role.or(prefix_role(prefix, text));
        if let Some(body_role) = end.and_then(|end| roles.get_mut(end)) {
            *body_role = item_role;
        }
        found.push(SpelledItem {
            item: Spelled {
                name: text_of(*name, text),
                start: tokens.get(first).map_or(0, Node::start_byte),
                end: body.map_or(name.end_byte(), Node::end_byte),
                visibility: prefix_visibility(prefix, text),
                role: item_role,
            },
            keyword,
            members,
            within: None,
            body: end.filter(|_| body.is_some()),
        });
    }
    Ok(found)
}

/// Give each braced tree right after an attribute (`#[cfg(test)] { .. }`)
/// the role the attribute adds to its own.
fn mark_attributed_trees(tokens: &[Node<'_>], roles: &mut [Role], text: &str) {
    for at in 2..tokens.len() {
        let spelled = |n: usize| tokens.get(n).map_or("", |token| text_of(*token, text));
        let attribute = spelled(at - 2) == "#" && spelled(at - 1).starts_with('[');
        if !attribute || !spelled(at).starts_with('{') {
            continue;
        }
        let added = tokens
            .get(at - 2..at)
            .map_or(Role::Code, |prefix| prefix_role(prefix, text));
        if let Some(role) = roles.get_mut(at) {
            *role = role.or(added);
        }
    }
}

/// Whether `keyword` and `name` open an item: an item keyword, then an
/// identifier.
fn starts_item(keyword: &str, name: Node<'_>, text: &str) -> bool {
    name.kind() == "identifier"
        && is_name(text_of(name, text))
        && DefinitionKind::of_keyword(keyword, Container::Items).is_some()
}

/// For each of `tokens`, the index of the first braced tree or `;` from
/// it on, `None` when the next item of the tree opens first: one pass
/// from the back, so a list of keywords costs no rescan.
fn item_ends(tokens: &[Node<'_>], text: &str) -> Vec<Option<usize>> {
    let mut ends = vec![None; tokens.len() + 1];
    for (at, token) in tokens.iter().enumerate().rev() {
        let spelled = text_of(*token, text);
        let own = (spelled == ";" || spelled.starts_with('{')).then_some(at);
        let next_item = tokens
            .get(at + 1)
            .is_some_and(|name| starts_item(spelled, *name, text));
        let next = ends.get(at + 1).copied().flatten().filter(|_| !next_item);
        if let Some(end) = ends.get_mut(at) {
            *end = own.or(next);
        }
    }
    ends
}

/// The named fields of a struct's or union's braces, or the variants of
/// an enum's, `body`; an enum variant's braces give its named fields.
fn members<'t>(
    body: Node<'_>,
    keyword: &str,
    text: &'t str,
    work: &mut Work,
) -> Result<Vec<Member<'t>>, IndexFault> {
    let entries = entries(body, text, work)?;
    let mut found = Vec::new();
    for entry in &entries {
        let Some(at) = entry_name(entry, text) else {
            continue;
        };
        let Some(name) = entry.get(at) else {
            continue;
        };
        let after = entry.get(at + 1).map_or("", |next| text_of(*next, text));
        let member = spelled_entry(entry, at, text);
        match keyword {
            "enum" => {
                let fields = match entry.get(at + 1).filter(|_| after.starts_with('{')) {
                    Some(group) => named_fields(*group, text, work)?,
                    None => Vec::new(),
                };
                found.push(Member {
                    member,
                    kind: DefinitionKind::Variant,
                    fields,
                });
            }
            "struct" | "union" if after == ":" && name.kind() == "identifier" => {
                found.push(Member {
                    member,
                    kind: DefinitionKind::Field,
                    fields: Vec::new(),
                });
            }
            _ => {}
        }
    }
    Ok(found)
}

/// The named fields of a braced `group`.
fn named_fields<'t>(
    group: Node<'_>,
    text: &'t str,
    work: &mut Work,
) -> Result<Vec<Spelled<'t>>, IndexFault> {
    let entries = entries(group, text, work)?;
    Ok(entries
        .iter()
        .filter_map(|entry| {
            let at = entry_name(entry, text)?;
            let after = entry.get(at + 1).map_or("", |next| text_of(*next, text));
            (after == ":").then(|| spelled_entry(entry, at, text))
        })
        .collect())
}

/// The entries of a braced `group`, its tokens split at each `,`.
fn entries<'n>(
    group: Node<'n>,
    text: &str,
    work: &mut Work,
) -> Result<Vec<Vec<Node<'n>>>, IndexFault> {
    let tokens = inner_children(group);
    work.spend(tokens.len())?;
    let mut entries = vec![Vec::new()];
    for token in tokens {
        if text_of(token, text) == "," {
            entries.push(Vec::new());
        } else if let Some(last) = entries.last_mut() {
            last.push(token);
        }
    }
    Ok(entries)
}

/// The index of an entry's name: its first token past its attributes and
/// visibility, when it is an identifier.
fn entry_name(entry: &[Node<'_>], text: &str) -> Option<usize> {
    let mut at = 0;
    loop {
        let token = |n: usize| entry.get(n).map_or("", |node| text_of(*node, text));
        at += match (token(at), token(at + 1)) {
            ("#", group) if group.starts_with('[') => 2,
            ("pub", group) if group.starts_with('(') => 2,
            ("pub", _) => 1,
            _ => break,
        };
    }
    entry
        .get(at)
        .filter(|name| name.kind() == "identifier")
        .map(|_| at)
}

/// The member an entry spells with its name at `at`: from its first token
/// to its last.
fn spelled_entry<'t>(entry: &[Node<'_>], at: usize, text: &'t str) -> Spelled<'t> {
    let prefix = entry.get(..at).unwrap_or_default();
    Spelled {
        name: entry.get(at).map_or("", |name| text_of(*name, text)),
        start: entry.first().map_or(0, Node::start_byte),
        end: entry.last().map_or(0, Node::end_byte),
        visibility: prefix_visibility(prefix, text),
        role: prefix_role(prefix, text),
    }
}

/// The visibility the tokens before an item's keyword state: a `pub` and
/// the group after it.
fn prefix_visibility(prefix: &[Node<'_>], text: &str) -> Visibility {
    let Some(at) = prefix
        .iter()
        .position(|token| text_of(*token, text) == "pub")
    else {
        return Visibility::Private;
    };
    // The group's tokens, so a comment in it changes nothing.
    let group: Vec<&str> = prefix
        .get(at + 1)
        .filter(|group| text_of(**group, text).starts_with('('))
        .map(|group| tokens_of(*group, text, usize::MAX))
        .unwrap_or_default()
        .into_iter()
        .map(|token| text_of(token, text))
        .collect();
    Visibility::of_modifier(&format!("pub {}", group.join(" "))).unwrap_or(Visibility::Public)
}

/// Test code when an attribute before an item's keyword makes it.
fn prefix_role(prefix: &[Node<'_>], text: &str) -> Role {
    prefix
        .windows(2)
        .filter(|pair| matches!(pair, [hash, _] if text_of(*hash, text) == "#"))
        .filter_map(|pair| pair.get(1))
        .map(|group| {
            let tokens: Vec<&str> = tokens_of(*group, text, usize::MAX)
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
