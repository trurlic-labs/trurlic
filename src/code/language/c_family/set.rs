//! The names a C or C++ file blanks. The tree's set is the fixpoint over
//! its headers' `#define`s ([`BlankMacros::of`]); a file blanks the part
//! of it that it writes, and its own `#define`s are judged with the
//! headers' of the same names ([`BlankMacros::of_file`]), so a flag a
//! source file defines blanks nothing in another file.
//!
//! A name is blankable when every `#define` of it is empty or only
//! specifiers, attribute forms and other blankable names, a function-like
//! one with at least one such body not empty (an empty one alone is a
//! function's stand-in in another arm); or when every `#define` of it is
//! only `namespace x {` openings or only closing braces, and the tree
//! defines such an opening.

use std::collections::{BTreeMap, BTreeSet};

use super::{Body, Define, Defines, Lexed, STATIC};

/// Each name's `#define`s: whether function-like, and the body.
type Bodies = BTreeMap<String, Vec<(bool, Body)>>;

/// What a group of `#define`s blanks: the names written alone, those of
/// them written `static`, and those called with arguments.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Settled {
    names: BTreeSet<String>,
    statics: BTreeSet<String>,
    calls: BTreeSet<String>,
    /// The steps its fixpoint took, which its inputs bound.
    steps: usize,
}

/// The tree's blank set: the `#define`s of its headers, by name, and what
/// they blank.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct BlankMacros {
    bodies: Bodies,
    settled: Settled,
    /// Whether a header defines a namespace's opening, which makes a
    /// closing brace blankable.
    opens: bool,
}

/// The names one C or C++ file blanks, and their identity, in its cache
/// key.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct FileMacros {
    pub(super) names: BTreeSet<String>,
    pub(super) statics: BTreeSet<String>,
    pub(super) calls: BTreeSet<String>,
    identity: [u8; 32],
}

impl BlankMacros {
    /// The set of a tree whose headers' `#define`s are `headers`, in work
    /// linear in them.
    pub(crate) fn of<'d>(headers: impl IntoIterator<Item = &'d Defines>) -> Self {
        let bodies = bodies_of(headers.into_iter().flat_map(|defines| &defines.0));
        let opens = bodies
            .values()
            .flatten()
            .any(|(_, body)| *body == Body::Opens);
        let settled = settle(&bodies, |_, _| false, opens);
        Self {
            bodies,
            settled,
            opens,
        }
    }

    /// What the file `lexed` describes blanks: each name it writes that
    /// the tree blanks, but those it defines itself, which are judged by
    /// its `#define`s and the headers' of the same names together.
    pub(crate) fn of_file(&self, lexed: &Lexed) -> FileMacros {
        let own: BTreeSet<&str> = lexed
            .defines
            .0
            .iter()
            .map(|define| define.name.as_str())
            .collect();
        let local = match own.is_empty() {
            true => Settled::default(),
            false => self.local(&lexed.defines, &own),
        };
        let mut file = FileMacros::default();
        for name in lexed.spelled() {
            let from = match own.contains(name) {
                true => &local,
                false => &self.settled,
            };
            for (set, into) in [
                (&from.names, &mut file.names),
                (&from.statics, &mut file.statics),
                (&from.calls, &mut file.calls),
            ] {
                if set.contains(name) {
                    into.insert(name.to_owned());
                }
            }
        }
        file.identity = identity(&file);
        file
    }

    /// What `defines`, a file's own, blank, judged with the headers'
    /// `#define`s of their names; a name `own` holds is never taken as
    /// settled from the tree.
    fn local(&self, defines: &Defines, own: &BTreeSet<&str>) -> Settled {
        let mut bodies = bodies_of(&defines.0);
        for (name, list) in &mut bodies {
            if let Some(tree) = self.bodies.get(name) {
                list.extend(tree.iter().cloned());
            }
        }
        let known = |name: &str, called: bool| {
            !own.contains(name)
                && match called {
                    true => self.settled.calls.contains(name),
                    false => self.settled.names.contains(name),
                }
        };
        settle(&bodies, known, self.opens)
    }
}

impl FileMacros {
    /// Its identity, in a cache key.
    pub(crate) const fn identity(&self) -> &[u8; 32] {
        &self.identity
    }
}

/// The identity of `file`'s names and their forms.
fn identity(file: &FileMacros) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    for (set, mark) in [
        (&file.names, b'n'),
        (&file.statics, b's'),
        (&file.calls, b'c'),
    ] {
        for name in set {
            hasher.update(&[mark]);
            hasher.update(&(name.len() as u64).to_le_bytes());
            hasher.update(name.as_bytes());
        }
    }
    *hasher.finalize().as_bytes()
}

/// `defines` by name.
fn bodies_of<'d>(defines: impl IntoIterator<Item = &'d Define>) -> Bodies {
    let mut bodies = Bodies::new();
    for define in defines {
        // clone: the map owns its names apart from the files'.
        let name = define.name.clone();
        // clone: and its bodies, which a file's own judgement reads again.
        let body = define.body.clone();
        bodies
            .entry(name)
            .or_default()
            .push((define.function_like, body));
    }
    bodies
}

/// What `bodies` blank, a name `known` holds in a form (`true` for called
/// with arguments) taken as blankable in it; `opens` says whether an
/// opening is defined beyond `bodies`.
fn settle(bodies: &Bodies, known: impl Fn(&str, bool) -> bool, opens: bool) -> Settled {
    let mut settled = Settled {
        names: namespaces(bodies, opens),
        ..Settled::default()
    };
    attributes(bodies, known, &mut settled);
    settled.statics = bodies
        .iter()
        .filter(|(name, list)| {
            name.len() >= STATIC.len() && settled.names.contains(*name) && holds_static(list)
        })
        // clone: the set owns the names apart from the map of bodies.
        .map(|(name, _)| name.clone())
        .collect();
    settled
}

/// Whether every body of `list` but an empty one holds `static`, one at
/// least.
fn holds_static(list: &[(bool, Body)]) -> bool {
    let mut held = false;
    for (_, body) in list {
        match body {
            Body::Attribute { empty: true, .. } => {}
            Body::Attribute {
                holds_static: true, ..
            } => held = true,
            Body::Attribute { .. } | Body::Opens | Body::Closes | Body::Code => return false,
        }
    }
    held
}

/// The names every `#define` of which opens or closes a namespace, when
/// one of them opens one or `opens` says another does; an empty arm
/// beside them is allowed.
fn namespaces(bodies: &Bodies, opens: bool) -> BTreeSet<String> {
    let openings = shaped(bodies, &Body::Opens);
    if openings.is_empty() && !opens {
        return openings;
    }
    openings
        .into_iter()
        .chain(shaped(bodies, &Body::Closes))
        .collect()
}

/// The names every `#define` of which is object-like and `wanted` or
/// empty, one at least `wanted`.
fn shaped(bodies: &Bodies, wanted: &Body) -> BTreeSet<String> {
    bodies
        .iter()
        .filter(|(_, list)| {
            list.iter()
                .all(|(function_like, body)| !function_like && (body == wanted || is_empty(body)))
                && list.iter().any(|(_, body)| body == wanted)
        })
        // clone: the set owns the names apart from the map of bodies.
        .map(|(name, _)| name.clone())
        .collect()
}

fn is_empty(body: &Body) -> bool {
    match body {
        Body::Attribute { empty, .. } => *empty,
        Body::Opens | Body::Closes | Body::Code => false,
    }
}

/// What a candidate name needs to be blanked: whether it is function-like,
/// and the names its bodies write, each blankable in the form it is
/// written in (`true` for called with arguments).
type Needs<'n> = (bool, BTreeSet<(&'n str, bool)>);

/// The attribute names of `bodies` added to `settled`: a least fixpoint
/// over a worklist, so a cycle blanks nothing and each name is settled
/// once. A name `known` holds needs nothing.
fn attributes(bodies: &Bodies, known: impl Fn(&str, bool) -> bool, settled: &mut Settled) {
    let needs = candidates(bodies, known, settled);
    let mut needed_by: BTreeMap<(&str, bool), Vec<&str>> = BTreeMap::new();
    let mut left: BTreeMap<&str, usize> = BTreeMap::new();
    let mut ready = Vec::new();
    for (name, (_, wants)) in &needs {
        for want in wants {
            needed_by.entry(*want).or_default().push(name);
        }
        left.insert(name, wants.len());
        if wants.is_empty() {
            ready.push(*name);
        }
    }
    while let Some(name) = ready.pop() {
        settled.steps += 1;
        let Some((function_like, _)) = needs.get(name) else {
            continue;
        };
        match function_like {
            true => settled.calls.insert(name.to_owned()),
            false => settled.names.insert(name.to_owned()),
        };
        for waiting in needed_by
            .remove(&(name, *function_like))
            .unwrap_or_default()
        {
            settled.steps += 1;
            if let Some(count) = left.get_mut(waiting) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    ready.push(waiting);
                }
            }
        }
    }
}

/// The candidates of `bodies`, by name, each with what it needs but the
/// names `known` holds; the steps it takes charged to `settled`.
fn candidates<'b>(
    bodies: &'b Bodies,
    known: impl Fn(&str, bool) -> bool,
    settled: &mut Settled,
) -> BTreeMap<&'b str, Needs<'b>> {
    let mut needs = BTreeMap::new();
    for (name, list) in bodies {
        if let Some((function_like, mut wants)) = candidate(list) {
            settled.steps += wants.len() + 1;
            wants.retain(|(want, called)| !known(want, *called));
            needs.insert(name.as_str(), (function_like, wants));
        }
    }
    needs
}

/// Whether the `#define`s `list` of one name make a candidate: its form
/// (function-like) and the names it needs blankable, each with the form
/// it is written in; `None` when no body or mix of them can be blanked.
fn candidate(list: &[(bool, Body)]) -> Option<Needs<'_>> {
    let function_like = list.first()?.0;
    let mut wants = BTreeSet::new();
    let mut written = false;
    for (form, body) in list {
        let Body::Attribute {
            names,
            calls,
            empty,
            ..
        } = body
        else {
            return None;
        };
        if *form != function_like {
            return None;
        }
        written |= !empty;
        wants.extend(names.iter().map(|name| (name.as_str(), false)));
        wants.extend(calls.iter().map(|name| (name.as_str(), true)));
    }
    (!function_like || written).then_some((function_like, wants))
}

#[cfg(test)]
impl BlankMacros {
    /// The steps the tree's fixpoint took.
    pub(super) const fn steps(&self) -> usize {
        self.settled.steps
    }
}
