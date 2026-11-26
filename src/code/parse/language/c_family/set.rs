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

use super::define::{Body, Define, Defines, Lexed, MAX_MACRO_BODY_TOKENS};
use crate::code::index::FileKind;
use crate::code::parse::language_of;

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
        .and_then(language_of)
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
    const fn steps(&self) -> usize {
        self.code.settled.steps
    }

    /// The steps the judgement of the file at `path` that `lexed`
    /// describes took.
    fn file_steps(&self, path: &str, lexed: &Lexed) -> usize {
        self.judged(path, lexed).1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::parse::language::c_family::lexed;
    use crate::code::parse::language::c_family::testing::{blanked, file, names, tree, tree_at};

    #[test]
    fn a_macro_is_blanked_when_every_definition_in_the_tree_is_an_attribute() {
        let header = "#if defined(_WIN32)\n#define API __declspec(dllexport)\n#else\n#define API __attribute__((visibility(\"default\")))\n#endif\n\
                  #define CONSTEXPR constexpr\n#define EMPTY\n#define NODISCARD [[nodiscard]]\n\
                  #define PRINTF(f, a) __attribute__((format(printf, f, a)))\n\
                  #define GUARDED(x) ANNOTATE(guarded_by(x))\n#define ANNOTATE(x) __attribute__((x))\n#define ANNOTATE(x)\n\
                  #define EXPORT API CONSTEXPR\n";
        let other = "#define CAPACITY 40\n#define HALF(n) ((n) / 2)\n#define SAME(x) x\n#define STUB(x)\n\
                 #define CALLED_ALONE ANNOTATE\n#define OUTSIDE SYSTEM_API\n#define CONSTEXPR 1\n";
        let all = "API CONSTEXPR EMPTY NODISCARD PRINTF GUARDED ANNOTATE EXPORT CAPACITY HALF SAME STUB CALLED_ALONE OUTSIDE";
        // `CONSTEXPR` is also `1`, so neither it nor `EXPORT`, which names it,
        // is blanked.
        let set = file(&tree(&[header, other]), all);

        assert_eq!(
            names(&set),
            (
                vec!["API", "EMPTY", "NODISCARD"],
                vec!["ANNOTATE", "GUARDED", "PRINTF"]
            )
        );
    }

    #[test]
    fn a_namespace_macro_is_blanked_when_the_tree_defines_an_opening() {
        let opening = "#define BEGIN namespace shelf { inline namespace v1 {\n#define END } }\n#define C_BEGIN extern \"C\" {\n";
        let closing_alone = "#define LOOP_END }\n";
        let all = "BEGIN END C_BEGIN LOOP_END";

        assert_eq!(
            names(&file(&tree(&[opening, closing_alone]), all)).0,
            ["BEGIN", "C_BEGIN", "END", "LOOP_END"]
        );
        assert!(file(&tree(&[closing_alone]), all).names.is_empty());
    }

    #[test]
    fn a_cycle_or_a_name_no_file_defines_blanks_nothing() {
        let set = file(
            &tree(&["#define A B\n#define B A\n#define C D_OUTSIDE\n#define E A\n"]),
            "A B C E",
        );

        assert!(set.names.is_empty() && set.calls.is_empty());
    }

    #[test]
    fn a_source_files_define_blanks_in_that_file_alone() {
        let local = "#define DONE\nDONE int f(void);\n";
        let other = "enum { PORT, DONE };\n";
        let headers = tree(&[]);

        assert_eq!(names(&file(&headers, local)).0, ["DONE"]);
        assert!(file(&headers, other).names.is_empty());
    }

    #[test]
    fn a_files_own_define_of_a_header_name_is_judged_with_the_headers() {
        let headers = tree(&["#define API\n#define EXPORT API\n"]);
        let redefined = "#undef API\n#define API 1\nAPI EXPORT int f(void);\n";

        assert_eq!(names(&file(&headers, redefined)).0, ["EXPORT"]);
        assert_eq!(names(&file(&headers, "API EXPORT")).0, ["API", "EXPORT"]);
    }

    #[test]
    fn a_files_own_define_naming_a_header_name_it_redefines_is_judged_by_its_own() {
        let headers = tree(&["#define API\n"]);
        let text = "#undef API\n#define API 1\n#define LOCAL API\nLOCAL int f(void);\n";

        assert!(file(&headers, text).names.is_empty());
    }

    #[test]
    fn a_files_own_closing_brace_is_blanked_when_a_header_opens_a_namespace() {
        let headers = tree(&["#define BEGIN namespace shelf {\n"]);

        assert_eq!(names(&file(&headers, "#define END }\nEND")).0, ["END"]);
        assert!(file(&tree(&[]), "#define END }\nEND").names.is_empty());
    }

    #[test]
    fn a_file_blanks_only_the_names_it_writes() {
        let text = "API int f(void);\n";
        let one = tree(&["#define API\n"]);
        let two = tree(&["#define API\n#define UNUSED\n"]);

        assert_eq!(file(&one, text), file(&two, text));
        assert_ne!(
            file(&one, text).identity(),
            file(&tree(&[]), text).identity()
        );
    }

    #[test]
    fn a_set_is_known_by_its_names_and_their_forms() {
        let alone = file(&tree(&["#define A\n"]), "A");
        let called = file(&tree(&["#define A(x) __attribute__((x))\n"]), "A");
        let static_one = file(&tree(&["#define ALWAYS_STATIC static\n"]), "ALWAYS_STATIC");
        let plain = file(&tree(&["#define ALWAYS_STATIC inline\n"]), "ALWAYS_STATIC");

        assert_ne!(alone.identity(), called.identity());
        assert_eq!(
            alone.identity(),
            file(&tree(&["#define A\n#define A\n"]), "A").identity()
        );
        assert_ne!(alone.identity(), FileMacros::default().identity());
        assert_ne!(static_one.identity(), plain.identity());
    }

    #[test]
    fn hostile_defines_are_read_in_linear_work() {
        let count = 100_000;
        let chain: String = (0..count)
            .map(|n| format!("#define M{n} M{}\n", n + 1))
            .chain([format!("#define M{count}\n")])
            .collect();
        let star: String = (0..count)
            .map(|n| format!("#define S{n} HUB\n"))
            .chain(["#define HUB\n".to_owned()])
            .collect();
        let cycle: String = (0..count)
            .map(|n| format!("#define C{n} C{}\n", (n + 1) % count))
            .collect();
        let long_body = format!("#define LONG {}\n", "inline ".repeat(1 << 17));
        let calls = format!(
            "#define ATTR(x) __attribute__((x))\n{}",
            "ATTR(".repeat(1 << 18)
        );

        for set in [&chain, &star, &cycle] {
            assert!(tree(&[set]).steps() <= 5 * (count + 1), "{}", &set[..20]);
        }
        assert!(tree(&[&long_body]).steps() == 0);
        assert_eq!(blanked(&[&calls], &calls).len(), calls.len());
    }

    #[test]
    fn a_files_own_defines_are_judged_in_work_its_own_bound() {
        let wide: String = (0..MAX_MACRO_BODY_TOKENS)
            .map(|n| format!("a{n} "))
            .collect();
        let headers: Vec<String> = (0..1000)
            .flat_map(|n| {
                [
                    format!("#define DEBUG {wide}\n"),
                    format!("#define WIDE w{n}\n"),
                ]
            })
            .collect();
        let headers: Vec<&str> = headers.iter().map(String::as_str).collect();
        let set = tree(&headers);
        let own = lexed(b"#define DEBUG\n#define WIDE\nDEBUG WIDE int f(void);\n");

        let steps: usize = (0..1000).map(|_| set.file_steps("src/f.c", &own)).sum();

        // Each file's two names, each needing at most the bound's names; the
        // thousand headers of each name cost nothing per file.
        assert!(steps <= 1000 * 4 * (2 + MAX_MACRO_BODY_TOKENS), "{steps}");
    }

    #[test]
    fn a_header_in_a_test_place_reaches_test_code_alone() {
        let set = tree_at(&[
            ("tests/fake.h", "#define DONE\n#define FAKE_API\n"),
            ("include/api.h", "#define API\n"),
        ]);
        let text = lexed(b"API FAKE_API DONE");
        let all = vec!["API", "DONE", "FAKE_API"];

        assert_eq!(names(&set.of_file("src/state.c", &text)).0, ["API"]);
        assert_eq!(names(&set.of_file("tests/state_check.c", &text)).0, all);
        assert_eq!(names(&set.of_file("src/state_test.cc", &text)).0, all);
    }
}
