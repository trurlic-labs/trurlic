//! The one matcher: which rules of a macro an invocation's or a
//! hand-off's arguments may open.
//!
//! It lines the arguments up with each rule's opening in order, from the
//! front, and from the back when both are whole, as far as no repetition
//! or fragment of many tokens stands between. Rust takes the first rule
//! the tokens match. So the candidates are each rule the arguments may
//! fit, up to the first they surely fit, and each rule matched is spent
//! from the file's budget.

use crate::code::index::{Element, Fragment, IndexFault, Rule};
use crate::code::work::Work;

/// How far arguments fit a rule's opening.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Fit {
    Refused,
    /// What lines up fits; a repetition or a fragment of many tokens
    /// leaves the rest untold.
    Possible,
    Sure,
}

/// The rules of `rules` that `arguments` may open, in order: each they
/// may fit, up to the first they surely fit. Each rule matched is spent
/// from `work`.
pub(super) fn candidates(
    rules: &[Rule],
    arguments: &[Element],
    whole: bool,
    work: &mut Work,
) -> Result<Vec<usize>, IndexFault> {
    let mut found = Vec::new();
    for (at, rule) in rules.iter().enumerate() {
        work.spend(1 + rule.opening.len().max(arguments.len()))?;
        match fit(rule, arguments, whole) {
            Fit::Refused => {}
            Fit::Possible => found.push(at),
            Fit::Sure => {
                found.push(at);
                break;
            }
        }
    }
    Ok(found)
}

/// Whether the binding at `place` of `rule` takes the name `arguments`
/// hold there: an `ident` or a `tt` binding, lined up with it.
pub(super) fn receives(rule: &Rule, arguments: &[Element], place: usize) -> bool {
    place < lined_up(&rule.opening, arguments)
        && matches!(
            rule.opening.get(place),
            Some(Element::Binding(Fragment::Ident | Fragment::Tree))
        )
}

/// How many elements of `opening` and `arguments` line up from the front:
/// up to the first that may stand for many tokens, on either side.
pub(super) fn lined_up(opening: &[Element], arguments: &[Element]) -> usize {
    opening
        .iter()
        .zip(arguments)
        .take_while(|(pattern, argument)| !is_open_ended(pattern, argument))
        .count()
}

/// Whether a pattern element and the argument beside it may take more
/// than one element or none: a repetition, a fragment of many tokens, or
/// a literal binding beside a minus, which a negative literal opens.
fn is_open_ended(pattern: &Element, argument: &Element) -> bool {
    may_be_empty(pattern)
        || matches!(argument, Element::Many)
        || (*pattern == Element::Binding(Fragment::Literal)
            && matches!(argument, Element::Token(token) if token == "-"))
}

/// How `arguments`, all of them when `whole`, fit `rule`'s opening: from
/// the front as far as the two line up, then from the back when both are
/// whole. Where one side ends for good and the other goes on, the rest
/// must be able to stand for nothing.
fn fit(rule: &Rule, arguments: &[Element], whole: bool) -> Fit {
    let opening = &rule.opening;
    let front = lined_up(opening, arguments);
    let mut fit = Fit::Sure;
    for (pattern, argument) in opening.iter().zip(arguments).take(front) {
        fit = fit.min(fits(pattern, argument));
    }
    if fit == Fit::Refused {
        return fit;
    }
    let opening_rest = opening.get(front..).unwrap_or_default();
    let arguments_rest = arguments.get(front..).unwrap_or_default();
    match (opening_rest.is_empty(), arguments_rest.is_empty()) {
        (true, true) if rule.whole && whole => fit,
        (true, false) if rule.whole && !arguments_rest.iter().any(may_be_empty) => Fit::Refused,
        (false, true) if whole && !opening_rest.iter().any(may_be_empty) => Fit::Refused,
        (false, false) if rule.whole && whole => {
            let room = opening_rest.len().min(arguments_rest.len());
            let back = opening_rest.iter().rev().zip(arguments_rest.iter().rev());
            for (pattern, argument) in back.take(room) {
                if is_open_ended(pattern, argument) {
                    break;
                }
                fit = fit.min(fits(pattern, argument));
            }
            fit.min(Fit::Possible)
        }
        (true | false, true | false) => fit.min(Fit::Possible),
    }
}

/// Whether `element` may stand for no tokens at all: a repetition, or a
/// fragment of many tokens, which `vis` may be.
fn may_be_empty(element: &Element) -> bool {
    matches!(element, Element::Many | Element::Binding(Fragment::Other))
}

/// How the pattern element `pattern` takes the argument `argument`, the
/// two lined up. A forwarded `ident`, `tt` or `lifetime` may match a
/// pattern's literal tokens; any other forwarded fragment is opaque and
/// matches only a binding.
fn fits(pattern: &Element, argument: &Element) -> Fit {
    let told = |same: bool| match same {
        true => Fit::Sure,
        false => Fit::Refused,
    };
    match pattern {
        Element::Word(word) => match argument {
            Element::Word(given) => told(word == given),
            Element::Forwarded(_, Fragment::Ident | Fragment::Tree) => Fit::Possible,
            Element::Forwarded(_, Fragment::Literal | Fragment::Lifetime | Fragment::Other)
            | Element::Token(_)
            | Element::Group(..) => Fit::Refused,
            Element::Binding(_) | Element::Many => Fit::Possible,
        },
        Element::Token(token) => match argument {
            Element::Token(given) => told(token == given),
            Element::Forwarded(_, Fragment::Tree | Fragment::Lifetime) => Fit::Possible,
            Element::Forwarded(_, Fragment::Ident | Fragment::Literal | Fragment::Other)
            | Element::Word(_)
            | Element::Group(..) => Fit::Refused,
            Element::Binding(_) | Element::Many => Fit::Possible,
        },
        Element::Group(open, tokens) => match argument {
            Element::Group(given, given_tokens) if open == given => match (tokens, given_tokens) {
                (Some(tokens), Some(given)) => told(tokens == given),
                (Some(_) | None, Some(_) | None) => Fit::Possible,
            },
            Element::Forwarded(_, Fragment::Tree) => Fit::Possible,
            Element::Forwarded(
                _,
                Fragment::Ident | Fragment::Literal | Fragment::Lifetime | Fragment::Other,
            )
            | Element::Group(..)
            | Element::Word(_)
            | Element::Token(_) => Fit::Refused,
            Element::Binding(_) | Element::Many => Fit::Possible,
        },
        Element::Binding(fragment) => binding(*fragment, argument),
        // An opening holds no forwarded metavariable; a repetition is
        // open-ended and never lined up.
        Element::Forwarded(..) | Element::Many => Fit::Possible,
    }
}

/// How a binding of `fragment` takes `argument`: a `tt` any one tree, an
/// `ident` a word, a `literal` a literal token or a boolean, each or its
/// own fragment forwarded. A `lifetime` binding is told from no token,
/// and a fragment of many tokens is open-ended.
fn binding(fragment: Fragment, argument: &Element) -> Fit {
    let told = |same: bool| match same {
        true => Fit::Sure,
        false => Fit::Refused,
    };
    match (fragment, argument) {
        (Fragment::Tree, _) => Fit::Sure,
        (Fragment::Ident, Element::Word(_)) => Fit::Sure,
        (Fragment::Literal, Element::Word(word)) => told(word == "true" || word == "false"),
        (Fragment::Literal, Element::Token(token)) => told(is_literal(token)),
        (Fragment::Ident | Fragment::Literal, Element::Forwarded(_, given)) => match given {
            Fragment::Tree => Fit::Possible,
            Fragment::Ident | Fragment::Literal | Fragment::Lifetime | Fragment::Other => {
                told(*given == fragment)
            }
        },
        (Fragment::Ident, Element::Token(_))
        | (Fragment::Ident | Fragment::Literal, Element::Group(..)) => Fit::Refused,
        // Arguments hold no binding, and a repetition is never lined up.
        (Fragment::Ident | Fragment::Literal, Element::Binding(_) | Element::Many)
        | (Fragment::Lifetime | Fragment::Other, _) => Fit::Possible,
    }
}

/// Whether `token` is a literal: a number, a string or a character, with
/// any prefix (`b"..."`, `r#"..."#`, `c"..."`).
fn is_literal(token: &str) -> bool {
    let unprefixed = token
        .trim_start_matches(['b', 'c', 'r'])
        .trim_start_matches('#');
    token.starts_with(|c: char| c.is_ascii_digit())
        || unprefixed.starts_with('"')
        || (unprefixed.starts_with('\'') && unprefixed.len() > 2 && unprefixed.ends_with('\''))
}
