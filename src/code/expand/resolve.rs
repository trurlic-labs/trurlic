//! What an invocation of a type-defining macro defines: the rules its
//! tokens may open, and what each defines the names it receives as,
//! following each hand-off of a name to the rules of another macro.
//!
//! One matcher (`fit.rs`) gives the rules an invocation opens and the
//! rules a hand-off reaches: the candidates, each rule the arguments may
//! fit up to the first they surely fit.
//!
//! A wrong definition is worse than a missing one. So a name is defined
//! only when every candidate defines it, with one kind, and its members
//! and items are those every candidate gives: the intersection across the
//! candidates of one hop, the union along a chain of hand-offs. A name
//! goes through at most [`MAX_HAND_OFFS`] hand-offs. What a rule defines
//! the name at a place as is computed once per file and number of hops
//! left, and every rule matched is spent from the file's budget.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::code::crates::Reach;
use crate::code::index::{Element, HandOff, IndexFault, Invocation, Rule, TraitToken, TypeMacro};
use crate::code::kinds::DefinitionKind;
use crate::code::rust::is_name;
use crate::code::symbol::Segment;
use crate::code::work::Work;

use super::fit::{candidates, receives};

/// The hand-offs a name may go through from the rule an invocation
/// opens; a chain longer gives nothing from past it, as a cycle of macros
/// handing a name round does not.
pub(in crate::code) const MAX_HAND_OFFS: usize = 8;

/// A macro a lookup finds: the macro, the file it is in, its place among
/// that file's macros, and the crate directory holding it.
#[derive(Clone, Copy)]
pub(in crate::code) struct Held<'i> {
    pub(in crate::code) found: &'i TypeMacro,
    pub(in crate::code) file: &'i str,
    pub(in crate::code) at: usize,
    pub(in crate::code) owner: &'i str,
}

/// An item a macro gives the type or the container: a name and its kind.
pub(in crate::code) type Item = (Arc<Segment>, DefinitionKind);

/// A member of a type: its trait's tokens for a trait impl's, and the
/// item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::code) struct Member<'i> {
    pub(in crate::code) trait_tokens: Option<&'i [TraitToken]>,
    pub(in crate::code) item: &'i Item,
}

/// What a name is defined as: its kind, when one is told, the type's
/// members, and the items of the invocation's container.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(in crate::code) struct Resolved<'i> {
    pub(in crate::code) kind: Option<DefinitionKind>,
    pub(in crate::code) members: BTreeSet<Member<'i>>,
    pub(in crate::code) items: BTreeSet<&'i Item>,
}

impl Resolved<'_> {
    /// What two candidates agree a name is: one kind, and the members and
    /// items both give.
    fn agree(mut self, other: &Self) -> Self {
        if self.kind != other.kind {
            self.kind = None;
        }
        self.members.retain(|member| other.members.contains(member));
        self.items.retain(|item| other.items.contains(item));
        self
    }
}

/// A rule's binding at a place, and the hand-offs it may still go
/// through: what one is defined as is a function of these alone.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct State<'i> {
    file: &'i str,
    at: usize,
    rule: usize,
    place: usize,
    hops: usize,
}

/// What a file's invocations define, each state computed once.
pub(in crate::code) struct Resolver<'s, 'i> {
    reach: &'s Reach<'s, 'i>,
    states: BTreeMap<State<'i>, Arc<Resolved<'i>>>,
    work: Work,
}

impl<'s, 'i> Resolver<'s, 'i> {
    /// A resolver of the macros `reach` finds, within `work`.
    pub(in crate::code) fn new(reach: &'s Reach<'s, 'i>, work: Work) -> Self {
        Self {
            reach,
            states: BTreeMap::new(),
            work,
        }
    }

    /// Each name `invocation` defines with a macro its file reaches, with
    /// what it is defined as, in the order of its arguments.
    pub(in crate::code) fn defined<'a>(
        &mut self,
        invocation: &'a Invocation,
    ) -> Result<Vec<(&'a str, Resolved<'i>)>, IndexFault> {
        let arguments = &invocation.arguments;
        let mut agreed: Option<BTreeMap<usize, (&'a str, Resolved<'i>)>> = None;
        for target in self.reach.named(&invocation.macro_name) {
            let rules = candidates(
                &target.found.rules,
                arguments,
                invocation.whole,
                &mut self.work,
            )?;
            for at in rules {
                let outcome = self.opened(target, at, arguments)?;
                agreed = Some(match agreed {
                    None => outcome,
                    Some(before) => agree_names(before, &outcome),
                });
            }
        }
        Ok(agreed.unwrap_or_default().into_values().collect())
    }

    /// The names the rule `at` of `target` defines of concrete
    /// `arguments`, by the place of each.
    fn opened<'a>(
        &mut self,
        target: Held<'i>,
        at: usize,
        arguments: &'a [Element],
    ) -> Result<BTreeMap<usize, (&'a str, Resolved<'i>)>, IndexFault> {
        let mut found = BTreeMap::new();
        let Some(rule) = target.found.rules.get(at) else {
            return Ok(found);
        };
        for (place, argument) in arguments.iter().enumerate() {
            let Element::Word(name) = argument else {
                continue;
            };
            if !is_name(name) || !receives(rule, arguments, place) {
                continue;
            }
            let state = self.state(target, at, place, MAX_HAND_OFFS)?;
            if state.kind.is_some() {
                found.insert(place, (name.as_str(), self.copied(&state)?));
            }
        }
        Ok(found)
    }

    /// What the binding at `place` of the rule `rule` of `held` is defined
    /// as, through at most `hops` hand-offs.
    fn state(
        &mut self,
        held: Held<'i>,
        rule: usize,
        place: usize,
        hops: usize,
    ) -> Result<Arc<Resolved<'i>>, IndexFault> {
        let key = State {
            file: held.file,
            at: held.at,
            rule,
            place,
            hops,
        };
        if let Some(found) = self.states.get(&key) {
            return Ok(Arc::clone(found));
        }
        let mut resolved = Resolved::default();
        if let Some(read) = held.found.rules.get(rule) {
            resolved = own(read, place);
            if let Some(left) = hops.checked_sub(1) {
                for hand_off in &read.hand_offs {
                    let Some(at) = received_at(hand_off, place) else {
                        continue;
                    };
                    let hop = self.hop(held, hand_off, at, left)?;
                    resolved.kind = resolved.kind.or(hop.kind);
                    self.work.spend(hop.members.len() + hop.items.len())?;
                    resolved.members.extend(hop.members);
                    resolved.items.extend(hop.items);
                }
            }
        }
        let resolved = Arc::new(resolved);
        self.states.insert(key, Arc::clone(&resolved));
        Ok(resolved)
    }

    /// What `hand_off`, in a rule of `from`, defines the name at `place`
    /// of its arguments as, through at most `hops` further hand-offs: what
    /// every rule it may reach agrees on, in the terms of `from`'s rule.
    fn hop(
        &mut self,
        from: Held<'i>,
        hand_off: &HandOff,
        place: usize,
        hops: usize,
    ) -> Result<Resolved<'i>, IndexFault> {
        let targets = match hand_off.from_crate {
            true => self.reach.in_crate(from.owner, &hand_off.macro_name),
            false => self.reach.named(&hand_off.macro_name),
        };
        let arguments = &hand_off.arguments;
        let mut agreed: Option<Resolved<'i>> = None;
        for target in targets {
            let rules = candidates(
                &target.found.rules,
                arguments,
                hand_off.whole,
                &mut self.work,
            )?;
            for at in rules {
                let outcome = match target.found.rules.get(at) {
                    Some(rule) if receives(rule, arguments, place) => {
                        let state = self.state(target, at, place, hops)?;
                        self.copied(&state)?
                    }
                    // A rule that takes the name as a literal word, or
                    // past what lines up, tells nothing of it.
                    Some(_) | None => Resolved::default(),
                };
                agreed = Some(match agreed {
                    None => outcome,
                    Some(before) => before.agree(&outcome),
                });
            }
        }
        Ok(agreed.unwrap_or_default())
    }

    /// A copy of `state`, which agreement with another candidate narrows,
    /// its members and items spent from the budget.
    fn copied(&mut self, state: &Resolved<'i>) -> Result<Resolved<'i>, IndexFault> {
        self.work.spend(state.members.len() + state.items.len())?;
        Ok(state.clone()) // clone: the state stays shared with every later reader
    }
}

/// What a rule's own body gives the binding at `place`: the kind it
/// spells, the members of its `impl`s, and its items.
fn own(rule: &Rule, place: usize) -> Resolved<'_> {
    let mut resolved = Resolved {
        items: rule.items.iter().collect(),
        ..Resolved::default()
    };
    let Some((_, receives)) = rule.receives.iter().find(|(at, _)| *at == place) else {
        return resolved;
    };
    resolved.kind = receives.kind;
    for found_impl in &receives.impls {
        for item in &found_impl.members {
            resolved.members.insert(Member {
                trait_tokens: found_impl.trait_tokens.as_deref(),
                item,
            });
        }
    }
    resolved
}

/// The place among `hand_off`'s arguments of the binding at `place` of
/// the rule it is in; `None` when it does not hand that binding on.
fn received_at(hand_off: &HandOff, place: usize) -> Option<usize> {
    hand_off
        .arguments
        .iter()
        .position(|argument| matches!(argument, Element::Forwarded(Some(at), _) if *at == place))
}

/// The names two candidates of one invocation agree on, by place: each
/// both define, with what they agree it is, of one kind.
fn agree_names<'a, 'i>(
    before: BTreeMap<usize, (&'a str, Resolved<'i>)>,
    other: &BTreeMap<usize, (&'a str, Resolved<'i>)>,
) -> BTreeMap<usize, (&'a str, Resolved<'i>)> {
    before
        .into_iter()
        .filter_map(|(place, (name, resolved))| {
            let (_, theirs) = other.get(&place)?;
            let agreed = resolved.agree(theirs);
            agreed.kind.map(|_| (place, (name, agreed)))
        })
        .collect()
}
