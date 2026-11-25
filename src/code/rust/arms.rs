//! What a `macro_rules!` macro's rules do with a name: for each rule, the
//! elements its pattern opens with, what each binding of one name among
//! them receives, and the macros its body hands a binding to.
//!
//! A rule's `$x:ident` or `$x:tt` defines a type of the kind of the type
//! keyword right before `$x` in its own body (`struct $x`). Its body may
//! also hand the binding to a macro it invokes at its top level:
//! `record!([..] $x)`, `$crate::record!($x)`, a recursive `m!(@inner
//! $x)`. Which rule of that macro receives it, and so what it defines, is
//! the crate's to say (`expand/resolve.rs`).

use tree_sitter::Node;

use crate::code::index::{Element, Fragment, HandOff, IndexFault, Receives, Rule};
use crate::code::kinds::{Container, DefinitionKind};
use crate::code::syntax::text_of;
use crate::code::work::Work;

use super::macros::{body_of, impls_of, inner_children, is_word, tokens_of, top_of};

/// The elements of a pattern's opening, of a hand-off's arguments and of
/// an invocation's that are kept: past them nothing is lined up.
pub(crate) const MAX_OPENING: usize = 16;

/// The tokens of a group that are kept to tell one rule from another
/// (`[summarized]` from `[compared]`); a group of more is one whose
/// tokens cannot be told.
pub(crate) const MAX_GROUP_TOKENS: usize = 8;

/// A keyword whose next token in a macro body is a type's name.
const TYPE_KEYWORDS: [&str; 5] = ["struct", "enum", "union", "trait", "type"];

/// The rules of a macro, `rules`. Each pattern and body is read a few
/// times at most and spent from `work`.
pub(super) fn rules_of(
    rules: &[Node<'_>],
    text: &str,
    work: &mut Work,
) -> Result<Vec<Rule>, IndexFault> {
    let mut read = Vec::with_capacity(rules.len());
    for rule in rules {
        let (opening, whole) = rule
            .child_by_field_name("left")
            .map(|pattern| opening(pattern, text))
            .unwrap_or_default();
        let body = rule.child_by_field_name("right");
        let tokens = body
            .map(|body| tokens_of(body, text, usize::MAX))
            .unwrap_or_default();
        work.spend(tokens.len())?;
        let top = match body {
            Some(body) => top_of(body, work)?,
            None => Vec::new(),
        };
        let parts = body_of(&top, text);
        let mut receives = Vec::new();
        for (place, (_, variable)) in opening.iter().enumerate() {
            let Some(variable) = variable.filter(|_| receives_a_name(&opening, place)) else {
                continue;
            };
            let found = Receives {
                kind: spelled_kind(&tokens, variable, text),
                impls: impls_of(&parts, variable, text, work)?,
            };
            if found != Receives::default() {
                receives.push((place, found));
            }
        }
        read.push(Rule {
            hand_offs: hand_offs(&top, &opening, text, work)?,
            opening: opening.into_iter().map(|(element, _)| element).collect(),
            whole,
            receives,
            items: parts.items,
        });
    }
    Ok(read)
}

/// An element of an opening, with the name of its binding when it is one
/// (`$name`).
type Opening<'t> = Vec<(Element, Option<&'t str>)>;

/// Whether the binding at `place` of `opening` can take a name: an
/// `ident` or a `tt`.
fn receives_a_name(opening: &Opening<'_>, place: usize) -> bool {
    matches!(
        opening.get(place),
        Some((Element::Binding(Fragment::Ident | Fragment::Tree), _))
    )
}

/// The kind of the type keyword right before `variable` in a body's
/// `tokens`.
fn spelled_kind(tokens: &[Node<'_>], variable: &str, text: &str) -> Option<DefinitionKind> {
    tokens.windows(2).find_map(|pair| {
        let [keyword, name] = pair else {
            return None;
        };
        let names = name.kind() == "metavariable" && text_of(*name, text) == variable;
        names.then(|| type_keyword(*keyword, text)).flatten()
    })
}

/// The elements `pattern` opens with, past the repetitions of attributes
/// it opens with (`$(#[$m:meta])*`, `$(#[doc = $d:literal])*`), at most
/// [`MAX_OPENING`], each with its binding's name, and whether they are all
/// of it.
fn opening<'t>(pattern: Node<'_>, text: &'t str) -> (Opening<'t>, bool) {
    let inside = inner_children(pattern);
    let rest = past_attributes(&inside, text);
    let kept = rest
        .iter()
        .take(MAX_OPENING)
        .map(|node| pattern_element(*node, text))
        .collect();
    (kept, rest.len() <= MAX_OPENING)
}

/// What a pattern's element is, with its binding's name.
fn pattern_element<'t>(node: Node<'_>, text: &'t str) -> (Element, Option<&'t str>) {
    match node.kind() {
        "token_binding_pattern" => {
            let fragment = node
                .child_by_field_name("type")
                .map_or("", |fragment| text_of(fragment, text));
            let name = node
                .child_by_field_name("name")
                .map(|name| text_of(name, text));
            (Element::Binding(fragment_of(fragment)), name)
        }
        "token_repetition_pattern" => (Element::Many, None),
        _ => (token_element(node, text), None),
    }
}

/// The fragment a binding's specifier names.
fn fragment_of(specifier: &str) -> Fragment {
    match specifier {
        "ident" => Fragment::Ident,
        "tt" => Fragment::Tree,
        "literal" => Fragment::Literal,
        "lifetime" => Fragment::Lifetime,
        // `expr`, `ty`, `path`, `pat`, `item`, `block`, `stmt`, `vis`,
        // `meta`: many tokens, or none.
        _ => Fragment::Other,
    }
}

/// A group by its delimiter and tokens, a word, or a token matching
/// itself.
pub(super) fn token_element(node: Node<'_>, text: &str) -> Element {
    let spelled = text_of(node, text);
    match (is_group(node), spelled.chars().next()) {
        (true, Some(open)) => Element::Group(open, group_tokens(node, text)),
        (true, None) => Element::Many,
        (false, _) if is_word(spelled) => Element::Word(spelled.to_owned()),
        (false, _) => Element::Token(spelled.to_owned()),
    }
}

/// Whether `node` is a delimited group of a token tree or a pattern.
fn is_group(node: Node<'_>) -> bool {
    matches!(node.kind(), "token_tree" | "token_tree_pattern")
}

/// The tokens a group holds when none is a group, a binding, a
/// repetition or a metavariable, at most [`MAX_GROUP_TOKENS`]; `None`
/// otherwise.
fn group_tokens(group: Node<'_>, text: &str) -> Option<Vec<String>> {
    let inside = inner_children(group);
    let literal = inside.len() <= MAX_GROUP_TOKENS
        && inside.iter().all(|token| {
            !is_group(*token)
                && !matches!(
                    token.kind(),
                    "metavariable"
                        | "token_binding_pattern"
                        | "token_repetition_pattern"
                        | "token_repetition"
                )
        });
    literal.then(|| {
        inside
            .iter()
            .map(|token| text_of(*token, text).to_owned())
            .collect()
    })
}

/// What an argument of a hand-off is, in a rule whose opening is
/// `opening`.
fn argument_element(node: Node<'_>, opening: &Opening<'_>, text: &str) -> Element {
    match node.kind() {
        "metavariable" => {
            let spelled = text_of(node, text);
            let bound = opening.iter().position(|(_, name)| *name == Some(spelled));
            let fragment = bound.and_then(|place| opening.get(place)).map_or(
                Fragment::Tree,
                |(element, _)| match element {
                    Element::Binding(fragment) => *fragment,
                    // A name binds no other element of an opening.
                    Element::Word(_)
                    | Element::Token(_)
                    | Element::Group(..)
                    | Element::Forwarded(..)
                    | Element::Many => Fragment::Tree,
                },
            );
            Element::Forwarded(bound, fragment)
        }
        "token_repetition" => Element::Many,
        _ => token_element(node, text),
    }
}

/// Each distinct macro invoked in `top`, a body's top-level tokens, with
/// a name binding of `opening` among its arguments.
fn hand_offs(
    top: &[Node<'_>],
    opening: &Opening<'_>,
    text: &str,
    work: &mut Work,
) -> Result<Vec<HandOff>, IndexFault> {
    let mut found: Vec<HandOff> = Vec::new();
    for (at, window) in top.windows(3).enumerate() {
        let [name, bang, tree] = window else {
            continue;
        };
        let invoked = name.kind() == "identifier"
            && text_of(*bang, text) == "!"
            && tree.kind() == "token_tree";
        if !invoked {
            continue;
        }
        let inside = inner_children(*tree);
        work.spend(inside.len())?;
        // The attributes a body passes on (`$(#[$doc])*`), as a pattern's
        // opening leaves them out.
        let arguments = past_attributes(&inside, text);
        let elements: Vec<Element> = arguments
            .iter()
            .take(MAX_OPENING)
            .map(|argument| argument_element(*argument, opening, text))
            .collect();
        let hands_a_name = elements.iter().any(|element| {
            matches!(element, Element::Forwarded(Some(place), _) if receives_a_name(opening, *place))
        });
        let hand_off = HandOff {
            macro_name: text_of(*name, text).to_owned(),
            from_crate: path_root(top, at, text) == "$crate",
            arguments: elements,
            whole: arguments.len() <= MAX_OPENING,
        };
        if hands_a_name && !found.contains(&hand_off) {
            found.push(hand_off);
        }
    }
    Ok(found)
}

/// The first segment of the path whose last, the macro's name, is
/// `top[at]`: `$crate` of `$crate::a::m`, `m` itself when it stands alone.
fn path_root<'t>(top: &[Node<'_>], mut at: usize, text: &'t str) -> &'t str {
    let spelled = |n: Option<usize>| n.and_then(|n| top.get(n)).map_or("", |t| text_of(*t, text));
    while spelled(at.checked_sub(1)) == "::" && !spelled(at.checked_sub(2)).is_empty() {
        at -= 2;
    }
    spelled(Some(at))
}

/// `tokens` past the repetitions of attributes they open with:
/// `$(#[$meta:meta])*` or `$(#[doc = $doc:literal])*` in a pattern,
/// `$(#[$meta])*` in a body. An invocation's attributes are left out alike
/// (`macros::invocation_arguments`).
fn past_attributes<'a, 'n>(tokens: &'a [Node<'n>], text: &str) -> &'a [Node<'n>] {
    let attributes = tokens
        .iter()
        .take_while(|token| {
            let spelled: String = text_of(**token, text)
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect();
            matches!(
                token.kind(),
                "token_repetition_pattern" | "token_repetition"
            ) && spelled.starts_with("$(#[")
                && spelled.ends_with("])*")
        })
        .count();
    tokens.get(attributes..).unwrap_or_default()
}

/// The kind a type keyword opens; `None` for any other token.
fn type_keyword(keyword: Node<'_>, text: &str) -> Option<DefinitionKind> {
    let keyword = text_of(keyword, text);
    TYPE_KEYWORDS
        .contains(&keyword)
        .then(|| DefinitionKind::of_keyword(keyword, Container::Items))
        .flatten()
}
