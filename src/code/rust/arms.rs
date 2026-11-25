//! What a `macro_rules!` macro's rules do with a name: for each rule, the
//! elements its pattern opens with, and for each `$x:ident` among them the
//! type its body spells of it and the macros it hands it to; and the arms
//! an invocation opens by literal words before the name.
//!
//! A rule's `$x:ident` defines a type of the kind of the type keyword
//! right before `$x` in its own body (`struct $x`). It also gives the name
//! to each macro its body invokes at its top level with `$x` among the
//! arguments: `record!([..] $x)`, `$crate::record!($x)`, a recursive
//! `m!(@inner $x)`. Which rule of that macro receives it, and so what it
//! defines, is the crate's to say (`expand/table.rs`). The arguments are
//! matched against each rule's opening in order, as Rust does.

use tree_sitter::Node;

use crate::code::index::{After, Element, HandOff, IndexFault, Receives, Rule, TypeArm};
use crate::code::kinds::{Container, DefinitionKind};
use crate::code::syntax::text_of;
use crate::code::work::Work;

use super::macros::{Body, body_of, impls_of, inner_children, is_word, tokens_of, top_of};

/// The literal words an arm may open with; an arm with more defines
/// nothing.
pub(crate) const MAX_ARM_WORDS: usize = 8;

/// The elements of a pattern's opening, and of a hand-off's arguments,
/// that are kept: a name past them is received by no rule.
pub(crate) const MAX_OPENING: usize = 16;

/// The literal tokens of a group that are kept to tell one rule from
/// another (`[summarized]` from `[compared]`); a group of more is taken
/// as one whose tokens cannot be told.
const MAX_GROUP_TOKENS: usize = 8;

/// A keyword whose next token in a macro body is a type's name.
const TYPE_KEYWORDS: [&str; 5] = ["struct", "enum", "union", "trait", "type"];

/// The arms and the rules of the macro of `rules`. Each pattern and body
/// is read once or twice and spent from `work`.
pub(super) fn rules_of(
    rules: &[Node<'_>],
    text: &str,
    work: &mut Work,
) -> Result<(Vec<TypeArm>, Vec<Rule>), IndexFault> {
    let mut arms = Vec::new();
    let mut read = Vec::with_capacity(rules.len());
    for (at, rule) in rules.iter().enumerate() {
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
        let read_body = ReadBody {
            tokens: &tokens,
            top: &top,
            parts: &parts,
        };
        let mut receives = Vec::new();
        for (place, element) in opening.iter().enumerate() {
            let variable = (element == &Element::Ident)
                .then(|| binding_name(rule, place, text))
                .flatten();
            if let Some(variable) = variable {
                receives.push((place, read_body.receives(variable, text, work)?));
            }
        }
        arms.extend(arm(&opening, whole, at));
        read.push(Rule {
            opening,
            whole,
            receives,
            items: parts.items.clone(),
        });
    }
    Ok((arms, read))
}

/// A rule's body as read once: its tokens, flat; its top-level tokens;
/// and the items and `impl`s at its top.
struct ReadBody<'a, 'n> {
    tokens: &'a [Node<'n>],
    top: &'a [Node<'n>],
    parts: &'a Body<'n>,
}

impl ReadBody<'_, '_> {
    /// What the body does with `variable`: the kind of the type keyword
    /// right before it, each top-level invocation that hands it on, and
    /// each `impl` of it.
    fn receives(
        &self,
        variable: &str,
        text: &str,
        work: &mut Work,
    ) -> Result<Receives, IndexFault> {
        let kind = self.tokens.windows(2).find_map(|pair| match pair {
            [keyword, name]
                if name.kind() == "metavariable" && text_of(*name, text) == variable =>
            {
                type_keyword(*keyword, text)
            }
            _ => None,
        });
        Ok(Receives {
            kind,
            hand_offs: hand_offs(self.top, variable, text, work)?,
            impls: impls_of(self.parts, variable, text, work)?,
        })
    }
}

/// The arm of the rule at `at` whose `opening` is literal words and then
/// an `$x:ident`; `whole` when the opening is the whole pattern.
fn arm(opening: &[Element], whole: bool, at: usize) -> Option<TypeArm> {
    let name = opening
        .iter()
        .position(|element| !matches!(element, Element::Word(_)))?;
    if opening.get(name) != Some(&Element::Ident) || name > MAX_ARM_WORDS {
        return None;
    }
    let words = opening
        .get(..name)?
        .iter()
        .filter_map(|element| {
            let Element::Word(word) = element else {
                return None;
            };
            Some(word.clone())
        })
        .collect();
    let after = match opening.get(name + 1) {
        Some(Element::Word(token) | Element::Token(token)) => After::Token(token.clone()),
        Some(Element::Group(open, _)) => After::Token(open.to_string()),
        Some(Element::Ident | Element::Tree | Element::Variable | Element::Many) => {
            After::Unchecked
        }
        None if whole => After::End,
        None => After::Unchecked,
    };
    Some(TypeArm {
        words,
        after,
        rule: at,
    })
}

/// The elements `pattern` opens with, past an opening `$(#[$m:meta])*`,
/// at most [`MAX_OPENING`], and whether they are all of it.
fn opening(pattern: Node<'_>, text: &str) -> (Vec<Element>, bool) {
    let inside = inner_children(pattern);
    let rest = match inside.split_first() {
        Some((first, rest)) if is_attribute_repetition(*first, text) => rest,
        Some(_) | None => inside.as_slice(),
    };
    let kept: Vec<Element> = rest
        .iter()
        .take(MAX_OPENING)
        .map(|node| pattern_element(*node, text))
        .collect();
    let whole = rest.len() <= MAX_OPENING;
    (kept, whole)
}

/// The name of the binding at `place` of the opening of `rule`.
fn binding_name<'t>(rule: &Node<'_>, place: usize, text: &'t str) -> Option<&'t str> {
    let pattern = rule.child_by_field_name("left")?;
    let inside = inner_children(pattern);
    let skip = usize::from(
        inside
            .first()
            .is_some_and(|first| is_attribute_repetition(*first, text)),
    );
    let node = inside.get(skip + place)?;
    Some(text_of(node.child_by_field_name("name")?, text))
}

/// What a pattern's element is.
fn pattern_element(node: Node<'_>, text: &str) -> Element {
    match node.kind() {
        "token_binding_pattern" => {
            let fragment = node
                .child_by_field_name("type")
                .map_or("", |fragment| text_of(fragment, text));
            match fragment {
                "ident" => Element::Ident,
                "tt" | "literal" | "lifetime" => Element::Tree,
                _ => Element::Many,
            }
        }
        "token_repetition_pattern" => Element::Many,
        _ => token_element(node, text),
    }
}

/// The tokens a group holds when each is a literal token, at most
/// [`MAX_GROUP_TOKENS`]; `None` otherwise.
fn literal_tokens(group: Node<'_>, text: &str) -> Option<Vec<String>> {
    let inside = inner_children(group);
    let literal = inside.len() <= MAX_GROUP_TOKENS
        && inside
            .iter()
            .all(|token| token.child_count() == 0 && token.kind() != "metavariable");
    literal.then(|| {
        inside
            .iter()
            .map(|token| text_of(*token, text).to_owned())
            .collect()
    })
}

/// What a hand-off's argument is.
fn argument_element(node: Node<'_>, text: &str) -> Element {
    match node.kind() {
        "metavariable" => Element::Variable,
        "token_repetition" => Element::Many,
        _ => token_element(node, text),
    }
}

/// A group by its delimiter and literal tokens, a word, or a token
/// matching itself.
fn token_element(node: Node<'_>, text: &str) -> Element {
    let spelled = text_of(node, text);
    match (node.child_count() > 0, spelled.chars().next()) {
        (true, Some(open)) => Element::Group(open, literal_tokens(node, text)),
        (true, None) => Element::Many,
        (false, _) if is_word(spelled) => Element::Word(spelled.to_owned()),
        (false, _) => Element::Token(spelled.to_owned()),
    }
}

/// Each macro invoked in `top`, a body's top-level tokens, with
/// `variable` among its arguments, with its arguments.
fn hand_offs(
    top: &[Node<'_>],
    variable: &str,
    text: &str,
    work: &mut Work,
) -> Result<Vec<HandOff>, IndexFault> {
    let mut found = Vec::new();
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
        let arguments = match inside.split_first() {
            Some((first, rest)) if is_passed_attributes(*first, text) => rest,
            Some(_) | None => inside.as_slice(),
        };
        let Some(place) = arguments.iter().position(|argument| {
            argument.kind() == "metavariable" && text_of(*argument, text) == variable
        }) else {
            continue;
        };
        if place >= MAX_OPENING {
            continue;
        }
        let from_crate = path_root(top, at, text) == "$crate";
        found.push(HandOff {
            macro_name: text_of(*name, text).to_owned(),
            from_crate,
            arguments: arguments
                .iter()
                .take(MAX_OPENING)
                .map(|argument| argument_element(*argument, text))
                .collect(),
            whole: arguments.len() <= MAX_OPENING,
            place,
        });
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

/// Whether `node` is `$(#[$doc])*`, the attributes a body hands on.
fn is_passed_attributes(node: Node<'_>, text: &str) -> bool {
    let spelled: String = text_of(node, text)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    node.kind() == "token_repetition" && spelled.starts_with("$(#[$") && spelled.ends_with("])*")
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

/// The kind a type keyword opens; `None` for any other token.
fn type_keyword(keyword: Node<'_>, text: &str) -> Option<DefinitionKind> {
    let keyword = text_of(keyword, text);
    TYPE_KEYWORDS
        .contains(&keyword)
        .then(|| DefinitionKind::of_keyword(keyword, Container::Items))
        .flatten()
}
