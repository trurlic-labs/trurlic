//! What a `macro_rules!` macro defines the name it is given as: for each
//! arm an invocation can open by its words, and for the macro as a whole,
//! which another macro reaches by handing it a name.
//!
//! An arm opens, after an optional `$(#[$meta:meta])*`, with at most
//! [`MAX_ARM_WORDS`] literal words and then a `$name:ident`. It defines a
//! type of the kind of the first type keyword before `$name` in its own
//! body (`struct $name`), or else in any body of the macro (a recursive
//! call reaches another arm); or else, when its own body hands `$name` to
//! a macro invocation (`record!([..] $name)`), what that macro defines it
//! as. The macro as a whole defines a name as the first type keyword
//! before a metavariable in any body, or else as the macro its first body
//! to hand on an `ident` metavariable hands it to. Which macro a name
//! reaches is the crate's to say (`expand.rs`), at a bound of hand-offs.

use std::collections::{BTreeMap, BTreeSet};

use tree_sitter::Node;

use crate::code::index::{Defines, IndexFault, TypeArm};
use crate::code::kinds::{Container, DefinitionKind};
use crate::code::syntax::text_of;
use crate::code::work::Work;

use super::macros::{inner_children, is_word, tokens_of};

/// The literal words an arm may open with; an arm with more defines
/// nothing.
pub(crate) const MAX_ARM_WORDS: usize = 8;

/// A keyword whose next token in a macro body is a type's name.
const TYPE_KEYWORDS: [&str; 5] = ["struct", "enum", "union", "trait", "type"];

/// What a macro's rules define: the arms an invocation opens by words,
/// and what the macro defines a name it is handed as.
pub(super) struct Defined {
    pub(super) arms: Vec<TypeArm>,
    pub(super) defines: Option<Defines>,
}

/// The rules of one macro as read once: each rule's pattern and body,
/// and each body's tokens.
struct Rules<'n> {
    patterns: Vec<Option<Node<'n>>>,
    bodies: Vec<Option<Node<'n>>>,
    tokens: Vec<Vec<Node<'n>>>,
}

/// What the macro of `rules`, whose `ident` metavariables are `names`,
/// defines. Each body's tokens are read twice at most and spent from
/// `work`.
pub(super) fn defined(
    rules: &[Node<'_>],
    names: &BTreeSet<&str>,
    text: &str,
    work: &mut Work,
) -> Result<Defined, IndexFault> {
    let mut read = Rules {
        patterns: Vec::with_capacity(rules.len()),
        bodies: Vec::with_capacity(rules.len()),
        tokens: Vec::with_capacity(rules.len()),
    };
    for rule in rules {
        let body = rule.child_by_field_name("right");
        let tokens = body
            .map(|body| tokens_of(body, text, usize::MAX))
            .unwrap_or_default();
        work.spend(tokens.len())?;
        read.patterns.push(rule.child_by_field_name("left"));
        read.bodies.push(body);
        read.tokens.push(tokens);
    }
    let all: Vec<&[Node<'_>]> = read.tokens.iter().map(Vec::as_slice).collect();
    let any_body = type_kinds(&all, text);
    let mut arms = Vec::new();
    for at in 0..rules.len() {
        if let Some(arm) = read.arm(at, &any_body, text, work)? {
            arms.push(arm);
        }
    }
    let first_kind = read
        .tokens
        .iter()
        .find_map(|tokens| first_type_kind(tokens, text));
    let defines = match first_kind {
        Some(kind) => Some(Defines::Kind(kind)),
        None => read.first_hand_off(|variable| names.contains(variable), text, work)?,
    };
    Ok(Defined { arms, defines })
}

impl Rules<'_> {
    /// The arm of rule `at`, when it opens by words and defines its name;
    /// `any_body` is the kind each metavariable has in any body.
    fn arm(
        &self,
        at: usize,
        any_body: &BTreeMap<&str, DefinitionKind>,
        text: &str,
        work: &mut Work,
    ) -> Result<Option<TypeArm>, IndexFault> {
        let own = self.tokens.get(at).map_or(&[][..], Vec::as_slice);
        // Each arm reads its own body once more: twice the macro in all.
        work.spend(own.len())?;
        let opening = self
            .patterns
            .get(at)
            .copied()
            .flatten()
            .and_then(|pattern| arm_opening(pattern, text));
        let Some((words, variable)) = opening else {
            return Ok(None);
        };
        let own_kinds = type_kinds(&[own], text);
        let kind = own_kinds
            .get(variable.as_str())
            .or_else(|| any_body.get(variable.as_str()));
        let defines = match kind {
            Some(kind) => Some(Defines::Kind(*kind)),
            None => {
                let body = self.bodies.get(at).copied().flatten();
                hand_off(body, |held| held == variable, text, work)?.map(Defines::HandedTo)
            }
        };
        Ok(defines.map(|defines| TypeArm { words, defines }))
    }

    /// The macro the first body that hands on a metavariable `handed`
    /// accepts hands it to.
    fn first_hand_off(
        &self,
        handed: impl Fn(&str) -> bool + Copy,
        text: &str,
        work: &mut Work,
    ) -> Result<Option<Defines>, IndexFault> {
        for body in self.bodies.iter().copied() {
            if let Some(name) = hand_off(body, handed, text, work)? {
                return Ok(Some(Defines::HandedTo(name)));
            }
        }
        Ok(None)
    }
}

/// The name of the first macro invoked at the top of `body` whose
/// arguments hold a metavariable `handed` accepts (`record!([] $name)`,
/// `path::record! { $name }`).
fn hand_off(
    body: Option<Node<'_>>,
    handed: impl Fn(&str) -> bool,
    text: &str,
    work: &mut Work,
) -> Result<Option<String>, IndexFault> {
    let top = body.map(inner_children).unwrap_or_default();
    for window in top.windows(3) {
        let [name, bang, tree] = window else {
            continue;
        };
        let invoked = name.kind() == "identifier"
            && text_of(*bang, text) == "!"
            && tree.kind() == "token_tree";
        if !invoked {
            continue;
        }
        let arguments = tokens_of(*tree, text, usize::MAX);
        work.spend(arguments.len())?;
        let holds = arguments
            .iter()
            .any(|token| token.kind() == "metavariable" && handed(text_of(*token, text)));
        if holds {
            return Ok(Some(text_of(*name, text).to_owned()));
        }
    }
    Ok(None)
}

/// The metavariables the patterns of `rules` bind as an `ident`
/// (`$name:ident`), at any depth of a repetition.
pub(super) fn ident_variables<'t>(
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

/// The kind of the first type keyword before a metavariable in `tokens`.
fn first_type_kind(tokens: &[Node<'_>], text: &str) -> Option<DefinitionKind> {
    tokens.windows(2).find_map(|pair| match pair {
        [keyword, name] if name.kind() == "metavariable" => type_keyword(*keyword, text),
        _ => None,
    })
}

/// The kind of the type each metavariable names in `bodies`: the first
/// type keyword before it; one pass over the tokens.
fn type_kinds<'t>(bodies: &[&[Node<'_>]], text: &'t str) -> BTreeMap<&'t str, DefinitionKind> {
    let mut kinds = BTreeMap::new();
    for pair in bodies.iter().flat_map(|tokens| tokens.windows(2)) {
        if let [keyword, name] = pair
            && name.kind() == "metavariable"
            && let Some(kind) = type_keyword(*keyword, text)
        {
            kinds.entry(text_of(*name, text)).or_insert(kind);
        }
    }
    kinds
}

/// The kind a type keyword opens; `None` for any other token.
fn type_keyword(keyword: Node<'_>, text: &str) -> Option<DefinitionKind> {
    let keyword = text_of(keyword, text);
    TYPE_KEYWORDS
        .contains(&keyword)
        .then(|| DefinitionKind::of_keyword(keyword, Container::Items))
        .flatten()
}
