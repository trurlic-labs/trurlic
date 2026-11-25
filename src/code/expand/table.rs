//! The arms an invocation can open, by macro name and words, and what
//! each defines its name as, following each hand-off of the name to the
//! rule of another macro that receives it.
//!
//! An invocation opens the arm of least place whose words it opens with.
//! The name after the words must be a word and no keyword, and the
//! pattern's token after the name must be the invocation's: `(compared
//! $name in ..)` is never opened by `front!(Plain in "m")`. The arm's rule
//! gives the name the kind of the type keyword before it in its own body,
//! else what the macros it hands the name to give it. A hand-off reaches
//! each rule of the named macro its arguments may fit, up to the first
//! they surely fit, as Rust takes the first rule an invocation matches.
//! The name goes on from there, at most [`MAX_HAND_OFFS`] hops and
//! [`MAX_RECEIVES`] rules in all. A crate's table is built and resolved
//! once and shared by its files. A file that holds macros of its own, one
//! an anchor brought from outside the walk, has a small table of its own,
//! looked up first.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::code::index::{After, Element, HandOff, Invocation, Receives, Rule, TypeArm, TypeMacro};
use crate::code::kinds::DefinitionKind;
use crate::code::rust::{MAX_ARM_WORDS, MAX_INVOCATION_TOKENS, is_name};

use super::{Defined, FOREIGN_TYPE_MACROS, argument};

/// The hand-offs a name may go through from the arm an invocation opens;
/// a chain longer gives no kind from past it, as a cycle of macros
/// handing a name round does not.
const MAX_HAND_OFFS: usize = 8;

/// The rules one arm's name may reach through its hand-offs, its own
/// included.
const MAX_RECEIVES: usize = 64;

/// The rules of a macro a hand-off is matched against before it is
/// taken to reach none.
const MAX_RULES_MATCHED: usize = 256;

/// A macro a table holds, the crate directory it is in, and whether a
/// workspace exports it to the table's crate rather than the crate
/// holding it.
#[derive(Clone, Copy)]
pub(in crate::code) struct Held<'i> {
    pub(in crate::code) found: &'i TypeMacro,
    pub(in crate::code) owner: &'i str,
    pub(in crate::code) exported: bool,
}

/// A name a rule receives: the macro, the rule's index among its rules,
/// and the place of the `$x:ident` that takes the name in its opening.
#[derive(Clone, Copy)]
struct Received<'i> {
    held: Held<'i>,
    rule: usize,
    place: usize,
}

/// What an arm defines its name as: the kind, and the rules that
/// received the name, each once, with what they give it: the type holds
/// their `impl`s' items, and the invocation's container their items.
#[derive(Clone)]
struct Resolved<'i> {
    kind: DefinitionKind,
    reached: Vec<(&'i Rule, &'i Receives)>,
}

/// An arm held, with what it defines once the table is resolved.
struct Entry<'i> {
    held: Held<'i>,
    arm: &'i TypeArm,
    resolved: Option<Resolved<'i>>,
}

/// The arms invocations of one crate, or one file, see, by macro name and
/// words, in the order a scan would meet them, and each macro by its name,
/// the first of a name, which a hand-off reaches.
#[derive(Default)]
pub(in crate::code) struct Arms<'i> {
    entries: Vec<Entry<'i>>,
    by_words: BTreeMap<(&'i str, &'i [String]), Vec<usize>>,
    by_name: BTreeMap<&'i str, Held<'i>>,
}

impl<'i> Arms<'i> {
    /// Hold the macro of `held`.
    pub(in crate::code) fn add(&mut self, held: Held<'i>) {
        let name = held.found.name.as_str();
        self.by_name.entry(name).or_insert(held);
        for arm in &held.found.arms {
            let at = self.entries.len();
            self.entries.push(Entry {
                held,
                arm,
                resolved: None,
            });
            self.by_words
                .entry((name, arm.words.as_slice()))
                .or_default()
                .push(at);
        }
    }

    /// The names of the macros held.
    pub(in crate::code) fn names(&self) -> BTreeSet<&'i str> {
        self.by_name.keys().copied().collect()
    }

    /// The macro of `name` held here.
    pub(in crate::code) fn held(&self, name: &str) -> Option<Held<'i>> {
        self.by_name.get(name).copied()
    }

    /// Resolve every arm, each hand-off reaching the macro `find` gives,
    /// from these tables, for the macro handing it on.
    pub(in crate::code) fn resolve(
        &mut self,
        find: impl Fn(&Self, Held<'i>, &HandOff) -> Option<Held<'i>>,
    ) {
        let within = |held: Held<'i>, hand_off: &HandOff| find(self, held, hand_off);
        let resolved: Vec<Option<Resolved<'i>>> = self
            .entries
            .iter()
            .map(|entry| {
                let start = Received {
                    held: entry.held,
                    rule: entry.arm.rule,
                    place: entry.arm.words.len(),
                };
                follow(start, &within)
            })
            .collect();
        for (entry, resolved) in self.entries.iter_mut().zip(resolved) {
            entry.resolved = resolved;
        }
    }

    /// The arm of least place `invocation` opens, with its place and the
    /// name after its words.
    fn opened<'a>(&self, invocation: &'a Invocation) -> Option<(usize, &'a String, &Entry<'i>)> {
        let tokens = &invocation.tokens;
        let mut best: Option<(usize, &'a String, &Entry<'i>)> = None;
        for words in 0..=MAX_ARM_WORDS.min(tokens.len()) {
            let Some(name) = tokens.get(words).filter(|name| is_name(name)) else {
                continue;
            };
            let key = (
                invocation.macro_name.as_str(),
                tokens.get(..words).unwrap_or_default(),
            );
            let found = self
                .by_words
                .get(&key)
                .into_iter()
                .flatten()
                .find_map(|at| {
                    let entry = self.entries.get(*at)?;
                    follows(&entry.arm.after, tokens.get(words + 1), tokens.len())
                        .then_some((*at, entry))
                });
            if let Some((at, entry)) = found
                && best.as_ref().is_none_or(|(place, ..)| at < *place)
            {
                best = Some((at, name, entry));
            }
        }
        best
    }
}

/// Whether the token after an invocation's name, `next`, of an
/// invocation that keeps `kept` tokens, is what `after` asks.
fn follows(after: &After, next: Option<&String>, kept: usize) -> bool {
    match after {
        After::End => next.is_none() && kept < MAX_INVOCATION_TOKENS,
        After::Token(token) => next == Some(token),
        After::Unchecked => true,
    }
}

/// What the name the rule `start` receives at its place is defined as:
/// the kind its own body spells, else the first a hand-off reaches,
/// nearest first; with every rule the name reached, each once, whether
/// the kind came from it or not.
fn follow<'i>(
    start: Received<'i>,
    find: &impl Fn(Held<'i>, &HandOff) -> Option<Held<'i>>,
) -> Option<Resolved<'i>> {
    let mut kind = None;
    let mut reached: Vec<(&'i Rule, &'i Receives)> = Vec::new();
    let mut seen: BTreeSet<(&str, &str, usize, usize)> = BTreeSet::new();
    let mut pending = VecDeque::from([(start, 0_usize)]);
    while let Some((Received { held, rule, place }, hops)) = pending.pop_front() {
        let key = (held.owner, held.found.name.as_str(), rule, place);
        if seen.len() == MAX_RECEIVES || !seen.insert(key) {
            continue;
        }
        let Some((rule, receives)) = held.found.rules.get(rule).and_then(|rule| {
            let (_, receives) = rule.receives.iter().find(|(at, _)| *at == place)?;
            Some((rule, receives))
        }) else {
            continue;
        };
        reached.push((rule, receives));
        kind = kind.or(receives.kind);
        if hops == MAX_HAND_OFFS {
            continue;
        }
        for hand_off in &receives.hand_offs {
            let Some(target) = find(held, hand_off) else {
                continue;
            };
            for rule in receiving_rules(&target.found.rules, hand_off) {
                let next = Received {
                    held: target,
                    rule,
                    place: hand_off.place,
                };
                pending.push_back((next, hops + 1));
            }
        }
    }
    kind.map(|kind| Resolved { kind, reached })
}

/// How far a hand-off's arguments fit a rule's opening.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Fit {
    No,
    /// What can be told fits; a binding of many tokens, a repetition or a
    /// group whose tokens are not all literal leaves the rest untold.
    Maybe,
    Yes,
}

/// The rules of `rules` that `hand_off` may reach, in order: each its
/// arguments may fit whose `$x:ident` at the name's place receives it, up
/// to the first they surely fit, as Rust takes the first rule an
/// invocation matches. A name a hand-off may give to several rules is
/// followed into each.
fn receiving_rules(rules: &[Rule], hand_off: &HandOff) -> Vec<usize> {
    let mut found = Vec::new();
    for (at, rule) in rules.iter().enumerate().take(MAX_RULES_MATCHED) {
        if rule.opening.get(hand_off.place) != Some(&Element::Ident) {
            continue;
        }
        match fit(rule, hand_off) {
            Fit::No => {}
            Fit::Maybe => found.push(at),
            Fit::Yes => {
                found.push(at);
                break;
            }
        }
    }
    found
}

/// How `hand_off`'s arguments fit `rule`'s opening, element by element; a
/// whole pattern and whole arguments end together.
fn fit(rule: &Rule, hand_off: &HandOff) -> Fit {
    let mut fit = Fit::Yes;
    let longest = rule.opening.len().max(hand_off.arguments.len());
    for at in 0..longest {
        let element = match (rule.opening.get(at), hand_off.arguments.get(at)) {
            (Some(Element::Many), _) | (_, Some(Element::Many)) => return fit.min(Fit::Maybe),
            (Some(_), Some(_)) if at == hand_off.place => Fit::Yes,
            (Some(pattern), Some(argument)) => fits(pattern, argument),
            (Some(_), None) if hand_off.whole => Fit::No,
            (None, Some(_)) if rule.whole => Fit::No,
            (Some(_), None) | (None, Some(_)) => Fit::Maybe,
            (None, None) => Fit::Yes,
        };
        fit = fit.min(element);
        if fit == Fit::No {
            return Fit::No;
        }
    }
    fit
}

/// How the pattern element `pattern` takes the hand-off argument
/// `argument`: a literal its own spelling, a group one of its delimiter
/// and tokens, an `ident` a word or a metavariable, a single-tree binding
/// any one element.
fn fits(pattern: &Element, argument: &Element) -> Fit {
    let told = |same: bool| match same {
        true => Fit::Yes,
        false => Fit::No,
    };
    match (pattern, argument) {
        (Element::Word(want), Element::Word(given))
        | (Element::Token(want), Element::Token(given)) => told(want == given),
        (Element::Group(open, Some(want)), Element::Group(given, Some(tokens))) => {
            told(open == given && want == tokens)
        }
        (Element::Group(open, _), Element::Group(given, _)) if open == given => Fit::Maybe,
        (Element::Ident, Element::Word(_) | Element::Variable) | (Element::Tree, _) => Fit::Yes,
        (Element::Word(_) | Element::Token(_) | Element::Group(..) | Element::Ident, _)
        | (Element::Variable | Element::Many, _) => Fit::No,
    }
}

/// The tables an invocation of one file is looked up in: its own macros'
/// first, then its crate's, whose workspace exports a name of its own
/// shadows.
#[derive(Clone, Copy)]
pub(in crate::code) struct Lookup<'a, 'i> {
    pub(in crate::code) own: Option<&'a Arms<'i>>,
    pub(in crate::code) shared: &'a Arms<'i>,
}

impl<'i> Lookup<'_, 'i> {
    /// Whether the file's own macros hold one of `name`.
    pub(in crate::code) fn own_holds(&self, name: &str) -> bool {
        self.own.is_some_and(|own| own.by_name.contains_key(name))
    }

    /// The name `invocation` defines with an arm of the tables, or with a
    /// foreign macro: the name, its kind and the macros it went through.
    /// An arm opened that defines nothing defines nothing, as the rule
    /// Rust takes is that arm's.
    pub(super) fn defined_by<'a>(&self, invocation: &'a Invocation) -> Option<Defined<'a, 'i>> {
        let own = self.own.and_then(|own| own.opened(invocation));
        let offset = self.own.map_or(0, |own| own.entries.len());
        let shared = self
            .shared
            .opened(invocation)
            .filter(|(_, _, entry)| {
                !(entry.held.exported && self.own_holds(&entry.held.found.name))
            })
            .map(|(place, name, entry)| (offset + place, name, entry));
        let opened = own
            .into_iter()
            .chain(shared)
            .min_by_key(|(place, ..)| *place);
        if let Some((_, name, entry)) = opened {
            let resolved = entry.resolved.clone()?;
            return Some(Defined {
                name,
                kind: resolved.kind,
                reached: resolved.reached,
            });
        }
        let (_, position, kind) = FOREIGN_TYPE_MACROS
            .iter()
            .find(|(name, ..)| *name == invocation.macro_name)?;
        argument(&invocation.tokens, *position).map(|name| Defined {
            name,
            kind: *kind,
            reached: Vec::new(),
        })
    }
}
