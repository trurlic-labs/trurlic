//! The names a C or C++ file blanks. A header's `#define`s reach every
//! file of the tree but one in a test place's: those reach only test
//! code, since what a test's fake or stub defines the code it tests never
//! sees. So a tree has two sets, each the fixpoint over the `#define`s that
//! reach its files ([`BlankMacros::of`]); a file blanks the part of its
//! set that it writes, and its own `#define`s are judged with the headers'
//! of the same names ([`BlankMacros::of_file`]), so a flag a source file
//! defines blanks nothing in another file.
//!
//! A name is blankable when every `#define` of it is empty or only
//! specifiers, attribute forms and other blankable names, a function-like
//! one with at least one such body not empty (an empty one alone is a
//! function's stand-in in another arm); or when every `#define` of it is
//! only `namespace x {` openings or only closing braces, and the tree
//! defines such an opening. The `#define`s of one name are read as one
//! [`Summary`], which a file's own of that name merge into, so a file's
//! judgement is linear in its own `#define`s, whatever the headers hold.

use std::collections::{BTreeMap, BTreeSet};

use super::{Body, Define, Defines, Lexed, MAX_MACRO_BODY_TOKENS};
use crate::code::index::FileKind;

/// What the `#define`s of one name are, read together: their forms, the
/// shapes of their bodies, and the names their attribute bodies need
/// blankable, each with its form (`true` for called with arguments). The
/// names past [`MAX_MACRO_BODY_TOKENS`] make the name no attribute, as a
/// body past it does.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Summary {
    object: bool,
    function: bool,
    /// One body no rule blanks, or names needed past the bound.
    code: bool,
    opens: bool,
    closes: bool,
    /// One attribute body not empty.
    written: bool,
    /// One body not empty without `static`, and one with it.
    plain: bool,
    statics: bool,
    wants: BTreeSet<(String, bool)>,
}

impl Summary {
    /// Read `define` into it, in work linear in its body's names.
    fn add(&mut self, define: &Define) {
        self.object |= !define.function_like;
        self.function |= define.function_like;
        match &define.body {
            Body::Code => self.code = true,
            Body::Opens => self.opens = true,
            Body::Closes => self.closes = true,
            Body::Attribute {
                names,
                calls,
                empty,
                holds_static,
            } => {
                self.written |= !empty;
                self.plain |= !empty && !holds_static;
                self.statics |= *holds_static;
                let needed = names.iter().map(|name| (name, false));
                for (name, called) in needed.chain(calls.iter().map(|name| (name, true))) {
                    if self.code {
                        break;
                    }
                    // The summary outlives the lexical passes it is read from.
                    // clone: so it owns its names.
                    self.wants.insert((name.clone(), called));
                    if self.wants.len() > MAX_MACRO_BODY_TOKENS {
                        self.code = true;
                        self.wants.clear();
                    }
                }
            }
        }
    }

    /// Whether its every body is an attribute.
    const fn attribute(&self) -> bool {
        !(self.code || self.opens || self.closes)
    }

    /// Its form when it may be blanked as an attribute (`true` for
    /// function-like): one form, and a function-like one written.
    const fn candidate(&self) -> Option<bool> {
        let one_form = self.object != self.function;
        match self.attribute() && one_form && (self.object || self.written) {
            true => Some(self.function),
            false => None,
        }
    }

    /// Whether every body is object-like and an opening (`opening`) or a
    /// closing, or empty, one at least of that shape.
    const fn shaped(&self, opening: bool) -> bool {
        let (wanted, other) = match opening {
            true => (self.opens, self.closes),
            false => (self.closes, self.opens),
        };
        wanted && !other && self.object && !self.function && !self.code && !self.written
    }

    /// Whether every body but an empty one holds `static`, one at least.
    const fn holds_static(&self) -> bool {
        self.attribute() && self.statics && !self.plain
    }
}

/// Each name's [`Summary`].
type Summaries = BTreeMap<String, Summary>;

/// What a group of `#define`s blanks: the names written alone, those of
/// them that make what follows private, and those called with arguments.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Settled {
    names: BTreeSet<String>,
    statics: BTreeSet<String>,
    calls: BTreeSet<String>,
    /// The steps its judgement took, which its inputs bound.
    steps: usize,
}

/// The set of the files one group of headers reaches: the summaries of
/// their `#define`s, what they blank, and whether one opens a namespace,
/// which makes a closing brace blankable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Reach {
    summaries: Summaries,
    settled: Settled,
    opens: bool,
}

/// The tree's blank sets: of its code, which the headers outside test
/// places reach, and of its test code, which every header reaches.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct BlankMacros {
    code: Reach,
    tests: Reach,
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
    /// The sets of a tree whose headers are `headers`, each its path and
    /// its `#define`s, in work linear in them.
    pub(crate) fn of<'d>(headers: impl IntoIterator<Item = (&'d str, &'d Defines)>) -> Self {
        let (mut code, mut tests) = (Summaries::new(), Summaries::new());
        for (path, defines) in headers {
            let test = is_test_code(path);
            for define in &defines.0 {
                // clone: each map owns its names apart from the passes.
                tests.entry(define.name.clone()).or_default().add(define);
                if !test {
                    // clone: and apart from the other map.
                    code.entry(define.name.clone()).or_default().add(define);
                }
            }
        }
        Self {
            code: Reach::of(code),
            tests: Reach::of(tests),
        }
    }

    /// What the file at `path` that `lexed` describes blanks: each name it
    /// writes that its set blanks, but those it defines itself, which are
    /// judged by its `#define`s and the headers' of the same names
    /// together.
    pub(crate) fn of_file(&self, path: &str, lexed: &Lexed) -> FileMacros {
        self.judged(path, lexed).0
    }

    /// [`Self::of_file`], and the steps the file's own `#define`s took.
    fn judged(&self, path: &str, lexed: &Lexed) -> (FileMacros, usize) {
        let reach = match is_test_code(path) {
            true => &self.tests,
            false => &self.code,
        };
        let own: BTreeSet<&str> = lexed
            .defines
            .0
            .iter()
            .map(|define| define.name.as_str())
            .collect();
        let local = match own.is_empty() {
            true => Settled::default(),
            false => reach.local(&lexed.defines, &own),
        };
        let mut file = FileMacros::default();
        for name in lexed.spelled() {
            let from = match own.contains(name) {
                true => &local,
                false => &reach.settled,
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
        (file, local.steps)
    }
}

impl Reach {
    fn of(summaries: Summaries) -> Self {
        let opens = summaries.values().any(|summary| summary.opens);
        let settled = settle(&summaries, |_, _| false, opens);
        Self {
            summaries,
            settled,
            opens,
        }
    }

    /// What `defines`, a file's own, blank, each name's summary merged
    /// into the headers' of it; a name `own` holds is never taken as
    /// settled from the headers.
    fn local(&self, defines: &Defines, own: &BTreeSet<&str>) -> Settled {
        let mut summaries = Summaries::new();
        let mut steps = 0;
        for define in &defines.0 {
            steps += 1;
            // The file's summary starts as the headers' of its name, at
            // most MAX_MACRO_BODY_TOKENS names.
            // clone: the map owns its names apart from the passes.
            let summary = summaries.entry(define.name.clone()).or_insert_with(|| {
                self.summaries
                    .get(&define.name)
                    .cloned()
                    .unwrap_or_default()
            });
            summary.add(define);
        }
        let known = |name: &str, called: bool| {
            !own.contains(name)
                && match called {
                    true => self.settled.calls.contains(name),
                    false => self.settled.names.contains(name),
                }
        };
        let mut settled = settle(&summaries, known, self.opens);
        settled.steps += steps;
        settled
    }
}

/// Whether the file at `path` is test code by its place.
fn is_test_code(path: &str) -> bool {
    FileKind::of(path)
        .and_then(FileKind::language)
        .is_some_and(|language| language.is_test_place(path))
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

/// What `summaries` blank, a name `known` holds in a form taken as
/// blankable in it; `opens` says whether an opening is defined beyond
/// them.
fn settle(summaries: &Summaries, known: impl Fn(&str, bool) -> bool, opens: bool) -> Settled {
    let shaped = |opening| {
        summaries
            .iter()
            .filter(move |(_, summary)| summary.shaped(opening))
            .map(|(name, _)| name)
    };
    let mut names: BTreeSet<String> = shaped(true).cloned().collect();
    if !names.is_empty() || opens {
        // clone: the set owns the names apart from the summaries.
        names.extend(shaped(false).cloned());
    }
    let mut settled = Settled {
        names,
        ..Settled::default()
    };
    attributes(summaries, known, &mut settled);
    settled.statics = summaries
        .iter()
        .filter(|(name, summary)| settled.names.contains(*name) && summary.holds_static())
        // clone: the set owns the names apart from the summaries.
        .map(|(name, _)| name.clone())
        .collect();
    settled
}

/// What a candidate name needs to be blanked: whether it is function-like,
/// and the names it needs blankable, each in its form.
type Needs<'n> = (bool, Vec<(&'n str, bool)>);

/// The candidates of `summaries`, by name, each with what it needs but the
/// names `known` holds; the steps it takes charged to `settled`.
fn candidates<'s>(
    summaries: &'s Summaries,
    known: impl Fn(&str, bool) -> bool,
    settled: &mut Settled,
) -> BTreeMap<&'s str, Needs<'s>> {
    let mut needs = BTreeMap::new();
    for (name, summary) in summaries {
        if let Some(function_like) = summary.candidate() {
            settled.steps += summary.wants.len() + 1;
            let wants = summary
                .wants
                .iter()
                .map(|(want, called)| (want.as_str(), *called))
                .filter(|(want, called)| !known(want, *called))
                .collect();
            needs.insert(name.as_str(), (function_like, wants));
        }
    }
    needs
}

/// The attribute names of `summaries` added to `settled`: a least
/// fixpoint over a worklist, so a cycle blanks nothing and each name is
/// settled once. A name `known` holds needs nothing.
fn attributes(summaries: &Summaries, known: impl Fn(&str, bool) -> bool, settled: &mut Settled) {
    let needs = candidates(summaries, known, settled);
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

#[cfg(test)]
impl BlankMacros {
    /// The steps the fixpoint of the tree's code took.
    pub(super) const fn steps(&self) -> usize {
        self.code.settled.steps
    }

    /// The steps the judgement of the file at `path` that `lexed`
    /// describes took.
    pub(super) fn file_steps(&self, path: &str, lexed: &Lexed) -> usize {
        self.judged(path, lexed).1
    }
}
