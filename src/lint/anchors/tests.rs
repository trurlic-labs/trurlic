use std::collections::{BTreeMap, BTreeSet};

use super::repair::{Search, Shown};
use super::*;
use crate::code::{Entry, FileFault, FileKind, ReadFault, Resolution};
use crate::store::StatusName;
use crate::store::testing::sample_decision;

/// The facts of a tree of `files`, each a path and its text, indexed by
/// the kind its name gives and taken by the walk, and of `other` entries
/// as they are, which only an anchor brought in.
fn facts(files: &[(&str, &str)], other: &[(&str, Entry)]) -> CodeFacts {
    let walked: BTreeSet<String> = files.iter().map(|(path, _)| (*path).to_owned()).collect();
    let mut entries: BTreeMap<String, Entry> = files
        .iter()
        .map(|(path, text)| {
            let kind = FileKind::of(path).unwrap();
            let file = Entry::File {
                kind,
                index: crate::code::testing::index(kind, text.as_bytes()).map_err(FileFault::Index),
            };
            ((*path).to_owned(), file)
        })
        .collect();
    entries.extend(
        other
            .iter()
            .map(|(path, entry)| ((*path).to_owned(), entry.clone())),
    );
    CodeFacts::new(entries, walked)
}

fn anchor(spelled: &str) -> Anchor {
    let (file, symbol) = match spelled.split_once("::") {
        Some((file, symbol)) => (file, Some(symbol.to_owned())),
        None => (spelled, None),
    };
    Anchor::new(file.to_owned(), symbol).unwrap()
}

/// A decision anchored at `anchors`, accepted or proposed.
fn decision(anchors: &[&str], status: StatusName) -> Decision {
    let mut decision = sample_decision("project");
    decision.anchors = anchors.iter().map(|spelled| anchor(spelled)).collect();
    decision.status = match status {
        StatusName::Accepted => Status::Accepted { via: None },
        StatusName::Proposed => Status::Proposed {
            accepted_text: None,
        },
    };
    decision
}

/// The findings of `decisions` as `check` prints them, after its label,
/// with git asked and `elsewhere` the other worktrees it gave, or, with
/// `None`, not asked.
fn judged(
    decisions: &[(&str, Decision)],
    here: &CodeFacts,
    elsewhere: Option<Vec<Elsewhere>>,
) -> Vec<String> {
    let named: Vec<(&str, &Decision)> = decisions.iter().map(|(n, d)| (*n, d)).collect();
    let here = Here::of(&named, here);
    let Some(elsewhere) = elsewhere else {
        return unasked(&named, &here)
            .iter()
            .map(ToString::to_string)
            .collect();
    };
    let mut asked = Asked::new(here.unresolved(&named));
    for tree in elsewhere {
        asked.read(tree);
    }
    findings(&named, &here, &asked)
        .iter()
        .map(ToString::to_string)
        .collect()
}

/// No other worktree: git asked, and it named none.
const ASKED: Option<Vec<Elsewhere>> = Some(Vec::new());

const LIB: &str = "pub struct Book { pages: u32 }\nimpl Book { fn pages(&self) -> u32 { self.pages } }\npub fn open() {}\n";

/// A second type with a field `pages`, which makes `pages` alone ambiguous
/// beside [`LIB`].
const SHELF: &str = "pub struct Shelf { pages: u32 }\n";

/// The verdict follows the cause, which it keeps: a file or a symbol
/// missing is stale on an accepted decision and planned on a proposed
/// one; a symbol out of the grammar or on a directory is malformed, and a
/// symbol on a file trurlic cannot read unverified, whatever the status.
#[test]
fn the_verdict_on_an_anchor_that_names_nothing_follows_its_cause() {
    let here = facts(
        &[("src/lib.rs", LIB), ("bad.json", "{")],
        &[
            ("docs", Entry::Directory),
            ("notes.txt", Entry::Unindexed),
            (
                "fifo.rs",
                Entry::File {
                    kind: FileKind::Rust,
                    index: Err(FileFault::Read(ReadFault::NotAFile)),
                },
            ),
        ],
    );
    let anchors = [
        "gone.rs",
        "src/lib.rs::close",
        "docs::intro",
        "notes.txt::line",
        "fifo.rs::read",
        "bad.json::key",
        "src/lib.rs::a::<B",
        "src/lib.rs::open",
        "docs",
    ];
    let decisions = [
        ("kept", decision(&anchors, StatusName::Accepted)),
        ("planned", decision(&anchors, StatusName::Proposed)),
    ];

    let missing = [
        ("gone.rs", "nothing is at its path"),
        (
            "src/lib.rs::close",
            "its file defines nothing its symbol names",
        ),
    ];
    let broken = [
        ("docs::intro", "its symbol is on a directory"),
        (
            "src/lib.rs::a::<B",
            "its symbol has an angle bracket that does not pair",
        ),
    ];
    let unread = [
        ("bad.json::key", "its file does not parse, near byte 1"),
        ("fifo.rs::read", "its file is not a regular file"),
        (
            "notes.txt::line",
            "its symbol is on a kind of file no resolver reads",
        ),
    ];
    let line = |name: &str, kind: &str, at: &str, cause: &str| {
        format!("anchor `{at}` of `{name}` is {kind}: {cause}")
    };
    let mut expected: Vec<String> = Vec::new();
    expected.extend(
        missing
            .iter()
            .map(|(at, why)| line("kept", "stale", at, why)),
    );
    for name in ["kept", "planned"] {
        expected.extend(
            broken
                .iter()
                .map(|(at, why)| line(name, "malformed", at, why)),
        );
    }
    for name in ["kept", "planned"] {
        expected.extend(unread.iter().map(|(at, why)| {
            let why = format!("{why}, so what it names is not verified");
            line(name, "unverified", at, &why)
        }));
    }
    expected.extend(
        missing
            .iter()
            .map(|(at, why)| line("planned", "planned", at, why)),
    );
    assert_eq!(judged(&decisions, &here, ASKED), expected);
}

/// With git not asked, an anchor that names nothing here on a decision
/// ever accepted is unresolved, stale and pending alike, its cause kept,
/// and one on a decision never accepted is no finding: only git tells
/// them apart. Ambiguous and weak need no git and are judged alike.
#[test]
fn with_git_not_asked_an_anchor_that_names_nothing_is_unresolved() {
    let here = facts(&[("src/lib.rs", &format!("{LIB}{SHELF}"))], &[]);
    let anchors = ["gone.rs", "src/lib.rs::close", "src/lib.rs::pages"];
    let decisions = [
        ("kept", decision(&anchors, StatusName::Accepted)),
        ("planned", decision(&anchors, StatusName::Proposed)),
    ];
    let holder = Elsewhere {
        worktree: Worktree {
            path: "/w/feature".into(),
            branch: Some("feature".to_owned()),
        },
        facts: facts(&[("src/lib.rs", "pub fn close() {}\n")], &[]),
        fork: None,
    };

    let unasked = judged(&decisions, &here, None);

    assert_eq!(
        unasked,
        [
            "anchor `gone.rs` of `kept` is unresolved: nothing is at its path in this tree",
            "anchor `src/lib.rs::close` of `kept` is unresolved: its file defines nothing its symbol names in this tree",
            "anchor `src/lib.rs::pages` of `kept` is ambiguous: its symbol names definitions of 2 paths",
            "anchor `src/lib.rs::pages` of `planned` is ambiguous: its symbol names definitions of 2 paths",
        ]
    );
    let asked = judged(&decisions, &here, Some(vec![holder]));
    let kinds: Vec<&str> = asked
        .iter()
        .map(|line| line.split(": ").next().unwrap())
        .collect();
    assert_eq!(
        kinds,
        [
            "anchor `gone.rs` of `kept` is stale",
            "anchor `src/lib.rs::pages` of `kept` is ambiguous",
            "anchor `src/lib.rs::pages` of `planned` is ambiguous",
            "anchor `src/lib.rs::close` of `kept` is pending",
            "anchor `gone.rs` of `planned` is planned",
            "anchor `src/lib.rs::close` of `planned` is planned",
        ]
    );
}

/// An anchor of a decision ever accepted that names nothing here and
/// names what another worktree holds is pending on the first such
/// worktree, a definition it names twice there and definitions of two
/// paths there included; one of a decision never accepted is planned,
/// wherever its code is.
#[test]
fn an_anchor_another_worktree_holds_is_pending_on_its_branch() {
    let here = facts(&[("src/lib.rs", LIB)], &[]);
    let tree = |path: &str, branch: Option<&str>, files: &[(&str, &str)]| Elsewhere {
        worktree: Worktree {
            path: path.into(),
            branch: branch.map(str::to_owned),
        },
        facts: facts(files, &[]),
        fork: None,
    };
    let elsewhere = [
        tree("/w/old", Some("old"), &[("src/lib.rs", LIB)]),
        tree(
            "/w/feature",
            Some("feature"),
            &[
                (
                    "src/lib.rs",
                    "pub fn close() {}\npub struct X;\nimpl X { fn twice(&self) {} }\nimpl X { fn twice(&self) {} }\nimpl X { fn run(&self) {} }\npub struct Y;\nimpl Y { fn run(&self) {} }\n",
                ),
                ("src/new.rs", ""),
            ],
        ),
        tree(
            "/w/detached",
            None,
            &[("src/lib.rs", "pub fn close() {}\npub fn late() {}\n")],
        ),
    ];
    let decisions = [
        (
            "accepted",
            decision(
                &[
                    "src/lib.rs::close",
                    "src/new.rs",
                    "src/lib.rs::twice",
                    "src/lib.rs::late",
                    "src/lib.rs::run",
                ],
                StatusName::Accepted,
            ),
        ),
        (
            "proposed",
            decision(
                &["src/lib.rs::late", "src/lib.rs::never"],
                StatusName::Proposed,
            ),
        ),
    ];

    let missing = "its file defines nothing its symbol names here";
    let planned = "its file defines nothing its symbol names";
    assert_eq!(
        judged(&decisions, &here, Some(elsewhere.into())),
        [
            format!("anchor `src/lib.rs::close` of `accepted` is pending: {missing}; it resolves in the worktree at /w/feature on branch `feature`"),
            format!("anchor `src/lib.rs::late` of `accepted` is pending: {missing}; it resolves in the worktree at /w/detached, whose HEAD is detached"),
            format!("anchor `src/lib.rs::run` of `accepted` is pending: {missing}; it resolves in the worktree at /w/feature on branch `feature`"),
            format!("anchor `src/lib.rs::twice` of `accepted` is pending: {missing}; it resolves in the worktree at /w/feature on branch `feature`"),
            "anchor `src/new.rs` of `accepted` is pending: nothing is at its path here; it resolves in the worktree at /w/feature on branch `feature`".to_owned(),
            format!("anchor `src/lib.rs::late` of `proposed` is planned: {planned}"),
            format!("anchor `src/lib.rs::never` of `proposed` is planned: {planned}"),
        ]
    );
}

/// Another worktree holds an anchor only when the commit its line forked
/// at did not: code the fork held is code the calling tree's line removed
/// since, which that worktree's older copy, edited elsewhere in the file,
/// brings back to nothing. Code the fork lacked is pending there.
#[test]
fn code_the_fork_held_is_stale_though_another_worktree_holds_it() {
    let here = facts(&[("src/lib.rs", "pub fn close() {}\n")], &[]);
    let there = Elsewhere {
        worktree: Worktree {
            path: "/w/fix".into(),
            branch: Some("fix".to_owned()),
        },
        facts: facts(
            &[(
                "src/lib.rs",
                "/// Reworded.\npub fn close() {}\npub fn gone() {}\npub fn fresh() {}\n",
            )],
            &[],
        ),
        fork: Some(facts(
            &[("src/lib.rs", "pub fn close() {}\npub fn gone() {}\n")],
            &[],
        )),
    };
    let decisions = [(
        "d",
        decision(
            &["src/lib.rs::gone", "src/lib.rs::fresh"],
            StatusName::Accepted,
        ),
    )];

    let kinds: Vec<String> = judged(&decisions, &here, Some(vec![there]))
        .iter()
        .map(|line| line.split(": ").next().unwrap().to_owned())
        .collect();

    assert_eq!(
        kinds,
        [
            "anchor `src/lib.rs::gone` of `d` is stale",
            "anchor `src/lib.rs::fresh` of `d` is pending",
        ]
    );
}

/// A symbol that names definitions of several paths is ambiguous with
/// their count; one that names definitions of one path and more than one
/// kind, a field and its getter, is no finding: the index makes them one
/// place on purpose.
#[test]
fn several_paths_are_ambiguous_and_one_path_of_several_kinds_is_no_finding() {
    let here = facts(&[("src/lib.rs", &format!("{LIB}{SHELF}"))], &[]);
    let decisions = [(
        "d",
        decision(
            &[
                "src/lib.rs::pages",
                "src/lib.rs::Book::pages",
                "src/lib.rs::Book",
            ],
            StatusName::Accepted,
        ),
    )];

    assert_eq!(
        judged(&decisions, &here, ASKED),
        ["anchor `src/lib.rs::pages` of `d` is ambiguous: its symbol names definitions of 2 paths",]
    );
}

/// A method named alone, which resolves through a suffix of its path.
const RUN_METHOD: &str = "pub struct R;\nimpl R { pub fn run() {} }\n";

/// A resolved symbol of one plain name that is not the whole path of what
/// it names is weak when [`WEAK_FILES`] or more walked files of its kind
/// hold a definition of it at any depth: a symbol that is the whole path
/// of a module-level item, a qualified symbol, a file anchor and a kind
/// that is no code never are.
#[test]
fn one_name_that_ten_files_of_its_kind_define_is_weak() {
    let mut files: Vec<(String, String)> = (0..WEAK_FILES - 1)
        .map(|n| {
            (
                format!("src/m{n}.rs"),
                "mod inner { struct S { run: u8 } }\npub fn top() {}\n".to_owned(),
            )
        })
        .collect();
    files.push(("src/lib.rs".to_owned(), RUN_METHOD.to_owned()));
    files.push(("src/top.rs".to_owned(), "pub fn top() {}\n".to_owned()));
    for n in 0..WEAK_FILES {
        files.push((format!("docs/d{n}.md"), "# run\n".to_owned()));
    }
    let borrowed: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    let here = facts(&borrowed, &[]);
    let decisions = [(
        "d",
        decision(
            &[
                "src/lib.rs::run",
                "src/top.rs::top",
                "src/m0.rs::S::run",
                "src/lib.rs",
                "docs/d0.md::run",
            ],
            StatusName::Accepted,
        ),
    )];

    assert_eq!(
        judged(&decisions, &here, ASKED),
        [format!(
            "anchor `src/lib.rs::run` of `d` is weak: its symbol is one name, defined in {WEAK_FILES} files of its kind"
        )]
    );
    let fewer: Vec<(&str, &str)> = borrowed
        .iter()
        .copied()
        .filter(|(path, _)| *path != "src/m1.rs")
        .collect();
    let below = facts(&fewer, &[]);
    assert_eq!(judged(&decisions, &below, ASKED), Vec::<String>::new());
}

/// The most severe kind first, then the decision, then the anchor; the
/// anchors [`Here::unresolved`] gives, those another worktree is asked
/// about, are those of decisions ever accepted alone.
#[test]
fn findings_sort_by_kind_then_decision_then_anchor() {
    let here = facts(&[("src/lib.rs", &format!("{LIB}{SHELF}"))], &[]);
    let decisions = [
        (
            "a",
            decision(&["src/lib.rs::pages", "z.rs", "y.rs"], StatusName::Proposed),
        ),
        (
            "b",
            decision(
                &["src/lib.rs::pages", "x.rs", "src/lib.rs::open"],
                StatusName::Accepted,
            ),
        ),
    ];

    let kinds: Vec<String> = judged(&decisions, &here, ASKED)
        .iter()
        .map(|line| line.split(": ").next().unwrap().to_owned())
        .collect();
    assert_eq!(
        kinds,
        [
            "anchor `x.rs` of `b` is stale",
            "anchor `src/lib.rs::pages` of `a` is ambiguous",
            "anchor `src/lib.rs::pages` of `b` is ambiguous",
            "anchor `y.rs` of `a` is planned",
            "anchor `z.rs` of `a` is planned",
        ]
    );
    let named: Vec<(&str, &Decision)> = decisions.iter().map(|(n, d)| (*n, d)).collect();
    let asked: Vec<String> = Here::of(&named, &here)
        .unresolved(&named)
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(asked, ["x.rs"]);
}

/// `check` fails on a stale, a malformed and an ambiguous anchor, warns
/// of an unverified or a weak one and notes a pending or a planned one.
#[test]
fn each_kind_has_its_severity() {
    let missing = || Missing(Resolution::FileMissing);
    let worktree = Worktree {
        path: "/w".into(),
        branch: None,
    };
    let kinds: [Kind<Checked>; 7] = [
        Kind::Missing(missing(), Checked::Stale { repair: None }),
        Kind::Malformed(Broken(Resolution::SymbolOnDirectory)),
        Kind::Ambiguous(2),
        Kind::Unverified(Unread(Resolution::NoResolver)),
        Kind::Weak(10),
        Kind::Missing(missing(), Checked::Pending(worktree)),
        Kind::Missing(missing(), Checked::Planned),
    ];
    let severities: Vec<Severity> = kinds.iter().map(Kind::severity).collect();
    assert_eq!(
        severities,
        [
            Severity::Error,
            Severity::Error,
            Severity::Error,
            Severity::Warning,
            Severity::Warning,
            Severity::Note,
            Severity::Note
        ]
    );
}

/// A proposed revision of an accepted decision has all its anchors judged
/// as the accepted decision's, since the accepted text keeps none: one
/// that names nothing is stale on it, as on the accepted one; only a
/// decision never accepted plans one.
#[test]
fn a_proposed_revision_has_its_anchors_judged_as_accepted() {
    use crate::store::AcceptedText;

    let here = facts(&[("src/lib.rs", LIB)], &[]);
    let mut revised = decision(&["src/lib.rs::close"], StatusName::Proposed);
    revised.status = Status::Proposed {
        accepted_text: Some(AcceptedText {
            choice: "The accepted choice".into(),
            reason: "The accepted reason".into(),
            alternatives: Vec::new(),
            via: None,
        }),
    };

    assert_eq!(
        judged(&[("revised", revised)], &here, ASKED),
        [
            "anchor `src/lib.rs::close` of `revised` is stale: its file defines nothing its symbol names"
        ]
    );
}

/// A file only an anchor brought into the facts, one the walk passed over,
/// is not counted: the count is the tree's whatever decisions are judged.
#[test]
fn a_file_no_walk_took_is_not_counted_for_weak() {
    let mut files: Vec<(String, String)> = (0..WEAK_FILES - 1)
        .map(|n| (format!("src/m{n}.rs"), "pub fn run() {}\n".to_owned()))
        .collect();
    files.push(("src/lib.rs".to_owned(), RUN_METHOD.to_owned()));
    let walked: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    let hidden = Entry::File {
        kind: FileKind::Rust,
        index: crate::code::testing::index(FileKind::Rust, b"pub fn run() {}\n")
            .map_err(FileFault::Index),
    };
    let here = facts(&walked[1..], &[(".hidden/x.rs", hidden)]);
    let decisions = [(
        "d",
        decision(
            &["src/lib.rs::run", ".hidden/x.rs::run"],
            StatusName::Accepted,
        ),
    )];

    assert_eq!(judged(&decisions, &here, ASKED), Vec::<String>::new());
    let all = facts(&walked, &[]);
    let one = [("d", decision(&["src/lib.rs::run"], StatusName::Accepted))];
    assert_eq!(judged(&one, &all, ASKED).len(), 1);
}

/// The manifest of a package at the root of the tree.
const MANIFEST: (&str, &str) = ("Cargo.toml", "[package]\nname = \"p\"\n");

const BOOK: &str = "pub struct Book;\nimpl Book { fn pages() {} }\n";

/// A clock whose `tick` is a method: its whole path is `Clock::tick`.
const CLOCK: &str = "pub struct Clock;\nimpl Clock { fn tick() {} }\n";

/// The searches of `decisions`' stale anchors in the tree `here`, the gone
/// files followed by `renames`, no other worktree holding any: each anchor
/// with the anchor it would be repaired as.
fn searches(
    decisions: &[(&str, Decision)],
    here: &CodeFacts,
    renames: &Renames,
) -> Vec<(String, String)> {
    let named: Vec<(&str, &Decision)> = decisions.iter().map(|(n, d)| (*n, d)).collect();
    let here = Here::of(&named, here);
    let asked = Asked::new(here.unresolved(&named));
    let found = repair::searched(&asked, &here, renames).into_iter();
    found
        .map(|search| (search.anchor.to_string(), search.there.to_string()))
        .collect()
}

/// A stale symbol anchor is searched for in the one other walked file of
/// its crate target, by place, that writes its symbol as an item's whole
/// path (a method named alone there is no namesake, nor a `use` of it),
/// and, when its file is gone and the history shows that file's blob at
/// another path, on that path whatever its kind or target. Not when its
/// file still defines its last segment (a trait method spelled without its
/// impl), nor when two files write it, nor on a file of another language;
/// never in another target (a test, a binary, a crate of its own), as a
/// re-export, a module declaration or an item a macro invocation produces;
/// and never with no manifest above the anchor.
#[test]
fn the_search_finds_one_file_of_the_anchors_target_or_its_followed_file() {
    let ticks = format!("{CLOCK}pub fn tick() {{}}\n");
    let here = facts(
        &[
            MANIFEST,
            (
                "src/lib.rs",
                "pub fn stay() {}\nmod refusal;\npub use refusal::Report;\n",
            ),
            ("src/moved.rs", BOOK),
            ("src/clock.rs", &ticks),
            ("src/twice_a.rs", "pub fn twice() {}\n"),
            ("src/twice_b.rs", "pub fn twice() {}\n"),
            ("src/moved.py", "def run():\n    pass\n"),
            (
                "src/spi.rs",
                "pub trait Routing { fn route_cost(&self) -> u8; }\n",
            ),
            (
                "src/fbqc.rs",
                "pub struct Fbqc;\nimpl crate::spi::Routing for Fbqc { fn route_cost(&self) -> u8 { 0 } }\n",
            ),
            (
                "src/refusal.rs",
                "mirror! { Report in \"m\" from other::Report {} }\n",
            ),
            ("src/cli.rs", "pub fn main() {}\n"),
            ("tests/cli.rs", "fn run(args: &[&str]) {}\n"),
            ("src/bin/tool.rs", "fn main() {}\nfn usage() {}\n"),
            ("src/api.rs", "pub use crate::inner::Engine;\n"),
            (
                "src/made.rs",
                "macro_rules! make { ($n:ident) => { pub struct $n; }; }\nmake!(Widget);\n",
            ),
            ("src/b/mod.rs", "mod scale;\n"),
            ("crates/b/Cargo.toml", "[package]\nname = \"b\"\n"),
            (
                "crates/b/src/lib.rs",
                "pub struct Config;\nimpl Config { pub fn new() {} }\n",
            ),
            ("conf/new.toml", "[profile]\npanic = 1\n"),
            ("lib/moved.rs", BOOK),
        ],
        &[],
    );
    let anchors = [
        "src/gone.rs::Book",
        "src/lib.rs::Book::pages",
        "src/lib.rs::tick",
        "src/lib.rs::twice",
        "src/gone.py::run",
        "src/fbqc.rs::Routing::route_cost",
        "src/refusal.rs::Report",
        "src/cli.rs::run",
        "src/cli.rs::usage",
        "src/cli.rs::Engine",
        "src/cli.rs::Widget",
        "src/a/scale.rs::scale",
        "crates/a/src/lib.rs::Config::new",
        "conf/old.toml::profile::panic",
        "lib/gone.rs::Book",
    ];
    let decisions = [("d", decision(&anchors, StatusName::Accepted))];
    let renames = Renames {
        to: BTreeMap::from([("conf/old.toml".to_owned(), "conf/new.toml".to_owned())]),
        facts: facts(&[], &[]),
    };

    let found = searches(&decisions, &here, &renames);

    let pair = |from: &str, to: &str| (from.to_owned(), to.to_owned());
    assert_eq!(
        found,
        [
            pair(
                "conf/old.toml::profile::panic",
                "conf/new.toml::profile::panic"
            ),
            pair("src/gone.rs::Book", "src/moved.rs::Book"),
            pair("src/lib.rs::Book::pages", "src/moved.rs::Book::pages"),
            pair("src/lib.rs::tick", "src/clock.rs::tick"),
        ]
    );
}

/// The anchor `src/a.rs::foo` searched on `src/b.rs`.
fn foo_search(anchor: &Anchor) -> Search<'_> {
    Search {
        anchor,
        there: self::anchor("src/b.rs::foo"),
    }
}

const FOO: &str = "pub fn foo() -> u32 { 1 }\n";

/// What the history held, each a name of a blob, a path and its text.
fn prior(files: &[(&str, &str, &str)]) -> Prior {
    let mut prior = Prior::default();
    prior.extend(files.iter().map(|&(name, path, text)| {
        let kind = FileKind::of(path).unwrap();
        let index = crate::code::testing::index(kind, text.as_bytes()).map_err(FileFault::Index);
        (
            name.to_owned(),
            path.to_owned(),
            Entry::File { kind, index },
        )
    }));
    prior
}

/// What the history shows of `src/b.rs`: `before` the loss, `after` it,
/// and after each change `since`, the anchor's file `lost` before it.
fn shown(before: Option<&str>, after: Option<&str>, since: &[&str]) -> Shown {
    Shown {
        lost: "lost".to_owned(),
        before: before.map(str::to_owned),
        after: after.map(str::to_owned),
        since: since.iter().map(|&name| name.to_owned()).collect(),
    }
}

/// A move is shown when the candidate went, across the loss, from writing
/// no item of the symbol to definitions of the span hash the anchor's had
/// before it, and held definitions of the kinds it gained after every
/// change since and holds them now, however edited: a namesake that stood
/// beside the code, other code of the name, one a later change deleted or
/// gave way to another kind of item and wrote again, or another kind now,
/// is no move. A loss in the work tree is judged against the tree's file.
#[test]
fn a_move_is_the_lost_code_arriving_where_it_was_not_and_kept_there() {
    let edited = "pub fn foo() -> u32 { 2 }\n";
    let other = "pub fn foo(x: u8) -> String { String::new() }\n";
    let lost = format!("{FOO}pub fn a() {{}}\n");
    let held = prior(&[
        ("lost", "src/a.rs", &lost),
        ("none", "src/b.rs", "pub fn b() {}\n"),
        ("beside", "src/b.rs", FOO),
        ("gained", "src/b.rs", FOO),
        ("other", "src/b.rs", other),
        ("edited", "src/b.rs", edited),
        ("dropped", "src/b.rs", "pub fn b() {}\n"),
        ("struct", "src/b.rs", "pub struct foo;\n"),
    ]);
    let tree = |text: &str| facts(&[MANIFEST, ("src/b.rs", text)], &[]);
    let anchor = anchor("src/a.rs::foo");
    let search = foo_search(&anchor);
    let cases = [
        (shown(Some("none"), Some("gained"), &[]), FOO, true),
        (shown(None, Some("gained"), &[]), FOO, true),
        (
            shown(Some("none"), Some("gained"), &["edited"]),
            edited,
            true,
        ),
        (shown(Some("beside"), Some("gained"), &[]), FOO, false),
        (shown(Some("none"), Some("other"), &[]), other, false),
        (
            shown(Some("none"), Some("gained"), &["dropped", "edited"]),
            edited,
            false,
        ),
        (
            shown(Some("none"), Some("gained"), &["struct", "gained"]),
            FOO,
            false,
        ),
        (
            shown(Some("none"), Some("gained"), &[]),
            "pub struct foo;\n",
            false,
        ),
        (shown(Some("none"), None, &[]), FOO, true),
        (shown(Some("beside"), None, &[]), FOO, false),
    ];

    for (at, (shown, now, moved)) in cases.into_iter().enumerate() {
        let now = tree(now);

        let repair = repair::moved(&search, &shown, &now, &held);

        let to = self::anchor("src/b.rs::foo");
        let expected = moved.then(|| Repair {
            code: now.code(&to).unwrap(),
            to,
        });
        assert_eq!(repair, expected, "case {at}");
    }
}

/// Where a file lost the code is the newest of its versions, each the file
/// before one of its changes, that wrote an item of the symbol.
#[test]
fn the_loss_is_the_newest_change_whose_file_before_wrote_the_item() {
    let held = prior(&[
        ("y^", "src/a.rs", "pub fn a() {}\n"),
        ("x^", "src/a.rs", FOO),
        ("w^", "src/a.rs", FOO),
    ]);
    let anchor = anchor("src/a.rs::foo");
    let search = foo_search(&anchor);

    assert_eq!(repair::lost(&search, ["y^", "x^", "w^"], &held), Some(1));
    assert_eq!(repair::lost(&search, ["y^"], &held), None);
}

/// A stale file anchor whose gone file the history shows moved unchanged
/// to another path is repaired there when that file defines an item: a
/// file of comments or nothing has no code to follow. A symbol anchor on
/// the file is no file anchor.
#[test]
fn a_file_anchor_follows_its_file_into_one_that_defines_an_item() {
    let here = facts(&[MANIFEST], &[]);
    let renamed = |from: &str, to: &str| (from.to_owned(), to.to_owned());
    let renames = Renames {
        to: BTreeMap::from([
            renamed("src/a.rs", "src/b.rs"),
            renamed("src/alpha.rs", "src/beta.rs"),
            renamed("src/void.rs", "src/empty.rs"),
        ]),
        facts: facts(
            &[
                ("src/b.rs", "pub fn b() {}\n"),
                ("src/beta.rs", "// License.\n"),
                ("src/empty.rs", ""),
            ],
            &[],
        ),
    };
    let anchors = [
        "src/a.rs",
        "src/a.rs::b",
        "src/alpha.rs",
        "src/void.rs",
        "src/never.rs",
    ];
    let decisions = [("d", decision(&anchors, StatusName::Accepted))];
    let named: Vec<(&str, &Decision)> = decisions.iter().map(|(n, d)| (*n, d)).collect();
    let here = Here::of(&named, &here);
    let asked = Asked::new(here.unresolved(&named));

    let followed = repair::followed(&asked, &here, &renames);

    let gone = anchor("src/a.rs");
    let to = anchor("src/b.rs");
    let code = renames.facts.code(&to).unwrap();
    assert_eq!(followed, BTreeMap::from([(&gone, Repair { to, code })]));
}

/// The role a stale anchor's code had is that of its definition where its
/// file is judged: a helper under `#[cfg(test)] mod tests` of a file of
/// code is test code, searched for in the test module it moved into.
#[test]
fn a_test_helper_follows_its_code_into_another_test_module() {
    let tests = "#[cfg(test)]\nmod tests { fn fixture() {} }\n";
    let landed = format!("pub fn b() {{}}\n{tests}");
    let here = facts(
        &[
            MANIFEST,
            ("src/a.rs", "pub fn a() {}\n"),
            ("src/b.rs", &landed),
        ],
        &[],
    );
    let decisions = [(
        "d",
        decision(&["src/a.rs::tests::fixture"], StatusName::Accepted),
    )];
    let lost = format!("pub fn a() {{}}\n{tests}");
    let held = prior(&[
        ("lost", "src/a.rs", &lost),
        ("none", "src/b.rs", "pub fn b() {}\n"),
        ("gained", "src/b.rs", &landed),
    ]);
    let anchor = anchor("src/a.rs::tests::fixture");
    let search = Search {
        anchor: &anchor,
        there: self::anchor("src/b.rs::tests::fixture"),
    };

    let found = searches(&decisions, &here, &Renames::none());
    let shown = shown(Some("none"), Some("gained"), &[]);
    let repair = repair::moved(&search, &shown, &here, &held);

    let to = "src/b.rs::tests::fixture".to_owned();
    assert_eq!(found, [("src/a.rs::tests::fixture".to_owned(), to.clone())]);
    assert_eq!(repair.map(|repair| repair.to.to_string()), Some(to));
}

/// Only a stale anchor is repaired: a planned one names what is not
/// written yet, and a pending one's code is on a branch. A proposed
/// revision of an accepted decision is stale, so searched, and the finding
/// shows its repair.
#[test]
fn a_planned_or_pending_anchor_is_never_repaired() {
    use crate::store::AcceptedText;

    let here = facts(
        &[
            MANIFEST,
            ("src/lib.rs", ""),
            ("src/moved.rs", "pub fn gone() {}\npub fn planned() {}\n"),
        ],
        &[],
    );
    let elsewhere = Elsewhere {
        worktree: Worktree {
            path: "/w/branch".into(),
            branch: Some("branch".into()),
        },
        facts: facts(&[("src/lib.rs", "pub fn branch() {}\n")], &[]),
        fork: None,
    };
    let mut revised = decision(&["src/lib.rs::gone"], StatusName::Proposed);
    revised.status = Status::Proposed {
        accepted_text: Some(AcceptedText {
            choice: "The accepted choice".into(),
            reason: "The accepted reason".into(),
            alternatives: Vec::new(),
            via: None,
        }),
    };
    let decisions = [
        (
            "planned",
            decision(&["src/lib.rs::planned"], StatusName::Proposed),
        ),
        (
            "pending",
            decision(&["src/lib.rs::branch"], StatusName::Accepted),
        ),
        ("revised", revised),
    ];
    let named: Vec<(&str, &Decision)> = decisions.iter().map(|(n, d)| (*n, d)).collect();
    let here = Here::of(&named, &here);
    let mut asked = Asked::new(here.unresolved(&named));
    asked.read(elsewhere);
    let found = repair::searched(&asked, &here, &Renames::none());
    let gone = anchor("src/lib.rs::gone");
    let to = anchor("src/moved.rs::gone");
    let code = here.facts.code(&to).unwrap();
    let asked = asked.with_repairs(BTreeMap::from([(&gone, Repair { to, code })]));

    let lines: Vec<String> = findings(&named, &here, &asked)
        .iter()
        .map(ToString::to_string)
        .collect();

    let searched: Vec<String> = found
        .iter()
        .map(|search| search.anchor.to_string())
        .collect();
    assert_eq!(searched, ["src/lib.rs::gone"]);
    assert_eq!(
        lines,
        [
            "anchor `src/lib.rs::gone` of `revised` is stale: its file defines nothing its \
             symbol names; repair: `src/moved.rs::gone`",
            "anchor `src/lib.rs::branch` of `pending` is pending: its file defines nothing its \
             symbol names here; it resolves in the worktree at /w/branch on branch `branch`",
            "anchor `src/lib.rs::planned` of `planned` is planned: its file defines nothing its \
             symbol names",
        ]
    );
}
