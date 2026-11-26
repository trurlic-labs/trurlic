use std::collections::{BTreeMap, BTreeSet};

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

/// The findings of `decisions` with `renames` and what git's history held,
/// `prior`, as `check` prints them, no other worktree holding any anchor:
/// the repairs composed as the shell composes them.
fn repaired(
    decisions: &[(&str, Decision)],
    facts: &CodeFacts,
    renames: Renames,
    prior: Prior,
) -> Vec<String> {
    let named: Vec<(&str, &Decision)> = decisions.iter().map(|(n, d)| (*n, d)).collect();
    let here = Here::of(&named, facts);
    let asked = asked_with_repairs(Asked::new(here.unresolved(&named)), &here, &renames, &prior);
    findings(&named, &here, &asked)
        .iter()
        .map(ToString::to_string)
        .collect()
}

/// `asked` with the repairs `renames` and `prior` show.
fn asked_with_repairs<'s>(
    asked: Asked<'s>,
    here: &Here<'s, '_>,
    renames: &Renames,
    prior: &Prior,
) -> Asked<'s> {
    let mut repairs = repair::followed(&asked, here, renames, prior);
    let searches = repair::searched(&asked, here, renames, &repairs, prior);
    let lost = repair::lost(searches, prior);
    repairs.extend(repair::moved(&lost, here.facts, prior));
    asked.with_repairs(repairs)
}

/// The commit every anchored file of these tests last changed in, so lost
/// its code in, and its parent.
const LOST: &str = "X";
const BEFORE: &str = "X^";

/// A later commit, and its parent.
const LATER: &str = "Y";
const BEFORE_LATER: &str = "Y^";

/// What `path` held at `revision`, `text`, as git's history gives it.
fn held(revision: &str, path: &str, text: &str) -> (String, String, Entry) {
    let kind = FileKind::of(path).unwrap();
    let index = crate::code::testing::index(kind, text.as_bytes()).map_err(FileFault::Index);
    (
        revision.to_owned(),
        path.to_owned(),
        Entry::File { kind, index },
    )
}

/// What git's history held at [`BEFORE`], each of `files` a path and its
/// text, every one last changed in [`LOST`].
fn prior(files: &[(&str, &str)]) -> Prior {
    let mut prior = Prior::default();
    prior.extend(files.iter().map(|(path, text)| held(BEFORE, path, text)));
    for (path, _) in files {
        prior.changed((*path).to_owned(), vec![LOST.to_owned()], false);
    }
    prior
}

/// `prior` with each of `paths` changed by `commit` and by no commit
/// since that added or deleted it.
fn arrived(prior: &mut Prior, commit: &str, paths: &[&str]) {
    for path in paths {
        prior.target_changed((*path).to_owned(), vec![(commit.to_owned(), true)]);
    }
}

/// A rename into `to`, made by [`LOST`].
fn renamed(to: &str, exact: bool) -> Renamed {
    Renamed {
        to: to.to_owned(),
        exact,
        before: BEFORE.to_owned(),
        made: Some(LOST.to_owned()),
    }
}

/// The manifest of a package at the root of the tree.
const MANIFEST: (&str, &str) = ("Cargo.toml", "[package]\nname = \"p\"\n");

const BOOK: &str = "pub struct Book;\nimpl Book { fn pages() {} }\n";

/// A clock whose `tick` is a method: its whole path is `Clock::tick`.
const CLOCK: &str = "pub struct Clock;\nimpl Clock { fn tick() {} }\n";

/// A stale symbol anchor is repaired to the one other file of its target
/// that writes its symbol as its whole path when git's history shows the
/// code moving there in one step: the commit where the anchor's file last
/// lost it, found before the file's later commits, gave the other file
/// definitions of the same span hash, which it did not write before (a
/// method named alone there is no namesake, and a `use` of the item gave
/// way to it). Not when two files define it, nor on a file of another
/// language, nor for any other cause.
#[test]
fn a_symbol_that_moved_to_one_other_file_is_repaired_there() {
    let ticks = format!("{CLOCK}pub fn tick() {{}}\n");
    let user = "pub fn t_other() -> u32 { foo() }\n";
    let here = facts(
        &[
            MANIFEST,
            ("src/lib.rs", "pub fn stay() {}\n"),
            ("src/moved.rs", BOOK),
            ("src/clock.rs", &ticks),
            ("src/t.rs", &format!("pub fn foo() -> u32 {{ 1 }}\n{user}")),
            ("src/twice_a.rs", "pub fn twice() {}\n"),
            ("src/twice_b.rs", "pub fn twice() {}\n"),
            ("src/moved.py", "def run():\n    pass\n"),
        ],
        &[("notes.txt", Entry::Unindexed)],
    );
    let anchors = [
        "src/lib.rs::Book::pages",
        "src/lib.rs::tick",
        "src/lib.rs::foo",
        "src/gone.rs::Book",
        "src/lib.rs::twice",
        "src/gone.py::run",
        "src/lib.rs::renamed_too",
        "notes.txt::line",
        "docs.md::Install",
    ];
    let decisions = [("d", decision(&anchors, StatusName::Accepted))];
    let history = || {
        let before =
            format!("pub fn stay() {{}}\npub fn tick() {{}}\npub fn foo() -> u32 {{ 1 }}\n{BOOK}");
        let mut prior = prior(&[
            ("src/gone.rs", BOOK),
            ("src/gone.py", "def run():\n    pass\n"),
            ("src/clock.rs", CLOCK),
            ("src/t.rs", &format!("use crate::lib::foo;\n{user}")),
        ]);
        prior.extend([
            held(BEFORE, "src/lib.rs", &before),
            held(BEFORE_LATER, "src/lib.rs", "pub fn stay() {}\n"),
            held(LOST, "src/moved.rs", BOOK),
            held(LOST, "src/clock.rs", &ticks),
            held(
                LOST,
                "src/t.rs",
                &format!("pub fn foo() -> u32 {{ 1 }}\n{user}"),
            ),
        ]);
        let commits = vec![LATER.to_owned(), LOST.to_owned()];
        prior.changed("src/lib.rs".to_owned(), commits, false);
        arrived(
            &mut prior,
            LOST,
            &["src/moved.rs", "src/clock.rs", "src/t.rs"],
        );
        prior
    };

    let lines = repaired(&decisions, &here, Renames::none(), history());
    let unshown = repaired(&decisions, &here, Renames::none(), Prior::default());

    let stale = |at: &str, cause: &str| format!("anchor `{at}` of `d` is stale: {cause}");
    let missing = "its file defines nothing its symbol names";
    let stales = |repairs: [&str; 4]| {
        let [book, pages, foo, tick] = repairs;
        vec![
            stale("docs.md::Install", "nothing is at its path"),
            stale("src/gone.py::run", "nothing is at its path"),
            stale(
                "src/gone.rs::Book",
                &format!("nothing is at its path{book}"),
            ),
            stale("src/lib.rs::Book::pages", &format!("{missing}{pages}")),
            stale("src/lib.rs::foo", &format!("{missing}{foo}")),
            stale("src/lib.rs::renamed_too", missing),
            stale("src/lib.rs::tick", &format!("{missing}{tick}")),
            stale("src/lib.rs::twice", missing),
        ]
    };
    let mut expected = stales([
        "; repair: `src/moved.rs::Book`",
        "; repair: `src/moved.rs::Book::pages`",
        "; repair: `src/t.rs::foo`",
        "; repair: `src/clock.rs::tick`",
    ]);
    let unverified = "anchor `notes.txt::line` of `d` is unverified: its symbol is on a kind of file no resolver reads, so what it names is not verified";
    expected.push(unverified.to_owned());
    assert_eq!(lines, expected);
    let mut expected = stales(["", "", "", ""]);
    expected.push(unverified.to_owned());
    assert_eq!(unshown, expected, "no history shows no move");
}

/// A namesake is no move: the search matches the symbol as a whole path,
/// so a bare name never lands on a type's method or a test module's
/// helper; a file that wrote the symbol before the loss, beside the code,
/// gained nothing; a different function of the same name written in the
/// commit that deleted the code, or after it, is not its code (a function
/// dropped from its file, a file deleted, a type of the library and one of
/// the binary); a symbol the anchor's file never defined has no code to
/// compare; a file the code moved into that a later commit deleted and
/// wrote again holds another file's namesake; and in the work tree, a
/// file that wrote the symbol at `HEAD` already gained nothing.
#[test]
fn a_namesake_that_stood_beside_the_code_is_no_move() {
    let here = facts(
        &[
            MANIFEST,
            ("src/cli.rs", "pub fn main() {}\n"),
            (
                "src/parse.rs",
                "pub fn parse() {}\n#[cfg(test)]\nmod tests { fn run() {} }\n",
            ),
            (
                "src/state.rs",
                "pub struct Bets;\nimpl Bets { fn waits_on(&self) {} }\n",
            ),
            ("src/mail.rs", "pub fn parse_header() {}\n"),
            ("src/store.rs", "pub fn write() {}\n"),
            (
                "src/net.rs",
                "pub fn retry_write(sock: u32) -> bool { true }\n",
            ),
            ("src/http.rs", "pub fn evict(idle: u8) {}\n"),
            ("src/lib.rs", "pub fn open() {}\n"),
            ("src/main.rs", "struct Config { port: u16 }\nfn main() {}\n"),
            ("src/phantom.rs", "pub fn phantom() {}\n"),
            ("src/local.rs", "pub fn other() {}\n"),
            ("src/shared.rs", "pub fn drafted() {}\n"),
            ("src/new.rs", "pub fn kept() -> String { String::new() }\n"),
        ],
        &[],
    );
    let anchors = [
        "src/cli.rs::run",
        "src/cli.rs::waits_on",
        "src/http_old.rs::parse_header",
        "src/store.rs::retry_write",
        "src/cache.rs::evict",
        "src/lib.rs::Config",
        "src/cli.rs::phantom",
        "src/local.rs::drafted",
        "src/w3.rs::kept",
    ];
    let decisions = [("d", decision(&anchors, StatusName::Accepted))];
    let mut history = prior(&[
        ("src/cli.rs", "pub fn run() {}\npub fn waits_on() {}\n"),
        ("src/http_old.rs", "pub fn parse_header() {}\n"),
        ("src/mail.rs", "pub fn parse_header() {}\n"),
        (
            "src/store.rs",
            "pub fn write() {}\npub fn retry_write(path: &str) -> bool { false }\n",
        ),
        ("src/cache.rs", "pub fn evict() {}\n"),
        ("src/w3.rs", "pub fn kept() {}\n"),
        ("src/lib.rs", "pub fn open() {}\npub struct Config;\n"),
    ]);
    history.extend([
        held(LOST, "src/mail.rs", "pub fn parse_header() {}\n"),
        held(
            LOST,
            "src/net.rs",
            "pub fn retry_write(sock: u32) -> bool { true }\n",
        ),
        held(LOST, "src/http.rs", "pub fn evict(idle: u8) {}\n"),
        held(
            LOST,
            "src/main.rs",
            "struct Config { port: u16 }\nfn main() {}\n",
        ),
        held(LOST, "src/phantom.rs", "pub fn phantom() {}\n"),
        held("HEAD", "src/local.rs", "pub fn drafted() {}\n"),
        held("HEAD", "src/shared.rs", "pub fn drafted() {}\n"),
        held(LOST, "src/new.rs", "pub fn kept() {}\n"),
    ]);
    history.changed("src/local.rs".to_owned(), Vec::new(), true);
    let replaced = vec![(LATER.to_owned(), true), (LOST.to_owned(), true)];
    history.target_changed("src/new.rs".to_owned(), replaced);

    arrived(
        &mut history,
        LOST,
        &[
            "src/mail.rs",
            "src/net.rs",
            "src/http.rs",
            "src/main.rs",
            "src/phantom.rs",
        ],
    );
    let lines = repaired(&decisions, &here, Renames::none(), history);

    assert!(
        lines.iter().all(|line| !line.contains("repair")),
        "{lines:#?}"
    );
    assert_eq!(lines.len(), anchors.len(), "{lines:#?}");
}

/// The search gives no repair where a reader would not accept one, git's
/// history showing the move or not: the anchor's own file still defines
/// its last segment (a trait method spelled without its impl, while the
/// trait declares it in another file), or spells it in a macro invocation
/// the index cannot expand, while another file re-exports it or writes
/// it; the one file that defines it is of another crate target (a test, a
/// binary, a crate of its own, vendored code with a manifest of its own),
/// defines it as test code for code, as an item of another kind (a type
/// alias for a struct), by a macro invocation, as a re-export or as a
/// module declaration; or no manifest the walk took is above the anchor,
/// so its crate is unknown.
#[test]
fn the_search_stays_in_the_anchors_own_crate_target_and_test_role() {
    let here = facts(
        &[
            MANIFEST,
            (
                "src/spi.rs",
                "pub trait Routing { fn route_cost(&self) -> u8; }\n",
            ),
            (
                "src/fbqc.rs",
                "pub struct Fbqc;\nimpl crate::spi::Routing for Fbqc { fn route_cost(&self) -> u8 { 0 } }\n",
            ),
            ("src/cli.rs", "pub fn main() {}\n"),
            (
                "src/refusal.rs",
                "mirror! { Report in \"m\" from other::Report {} }\n",
            ),
            ("src/lib.rs", "mod refusal;\npub use refusal::Report;\n"),
            ("src/spelled.rs", "unknown! { Gadget }\n"),
            ("src/gadget.rs", "pub struct Gadget;\n"),
            ("tests/cli.rs", "fn run(args: &[&str]) {}\n"),
            ("src/bin/tool.rs", "fn main() {}\nfn usage() {}\n"),
            ("src/fixtures.rs", "#[cfg(test)]\npub fn sample() {}\n"),
            ("src/api.rs", "pub use crate::inner::Engine;\n"),
            (
                "src/made.rs",
                "macro_rules! make { ($n:ident) => { pub struct $n; }; }\nmake!(Widget);\n",
            ),
            ("src/b/mod.rs", "mod scale;\n"),
            ("src/b/scale.rs", "pub fn estimate() {}\n"),
            ("src/units.rs", "pub type Meters = u32;\n"),
            ("crates/b/Cargo.toml", "[package]\nname = \"b\"\n"),
            (
                "crates/b/src/lib.rs",
                "pub struct Config;\nimpl Config { pub fn new() {} }\n",
            ),
            ("src/de.rs", "pub trait Other {}\n"),
            ("vendor/serde/Cargo.toml", "[package]\nname = \"serde\"\n"),
            (
                "vendor/serde/src/de.rs",
                "pub trait Deserializer { fn deserialize_any(self); }\n",
            ),
        ],
        &[],
    );
    let anchors = [
        "src/fbqc.rs::Routing::route_cost",
        "src/cli.rs::run",
        "src/refusal.rs::Report",
        "src/spelled.rs::Gadget",
        "crates/a/src/lib.rs::Config::new",
        "src/de.rs::Deserializer::deserialize_any",
        "src/cli.rs::usage",
        "src/cli.rs::sample",
        "src/cli.rs::Widget",
        "src/cli.rs::Engine",
        "src/a/scale.rs::scale",
        "src/cli.rs::Meters",
    ];
    let decisions = [("d", decision(&anchors, StatusName::Accepted))];
    let mut history = prior(&[
        (
            "src/fbqc.rs",
            "pub trait Routing { fn route_cost(&self) -> u8; }\n",
        ),
        (
            "src/cli.rs",
            "pub fn run() {}\nfn usage() {}\npub fn sample() {}\npub struct Widget;\npub struct Engine;\npub struct Meters;\n",
        ),
        ("src/refusal.rs", "pub struct Report;\n"),
        ("src/spelled.rs", "pub struct Gadget;\n"),
        (
            "crates/a/src/lib.rs",
            "pub struct Config;\nimpl Config { pub fn new() {} }\n",
        ),
        (
            "src/de.rs",
            "pub trait Deserializer { fn deserialize_any(self); }\n",
        ),
        ("src/a/scale.rs", "pub fn scale() {}\n"),
        ("src/b/mod.rs", ""),
    ]);
    history.extend([
        held(LOST, "src/gadget.rs", "pub struct Gadget;\n"),
        held(
            LOST,
            "src/fixtures.rs",
            "#[cfg(test)]\npub fn sample() {}\n",
        ),
        held(LOST, "src/b/mod.rs", "mod scale;\n"),
        held(LOST, "src/units.rs", "pub type Meters = u32;\n"),
    ]);

    let targets = [
        "src/gadget.rs",
        "src/fixtures.rs",
        "src/b/mod.rs",
        "src/units.rs",
    ];
    arrived(&mut history, LOST, &targets);
    let lines = repaired(&decisions, &here, Renames::none(), history);

    assert!(
        lines.iter().all(|line| !line.contains("repair")),
        "{lines:#?}"
    );
    assert_eq!(lines.len(), anchors.len(), "{lines:#?}");
    let unknown = facts(&[("lib/moved.rs", BOOK)], &[]);
    let lone = [("d", decision(&["lib/gone.rs::Book"], StatusName::Accepted))];
    let mut history = prior(&[("lib/gone.rs", BOOK)]);
    history.extend([held(LOST, "lib/moved.rs", BOOK)]);
    assert_eq!(
        repaired(&lone, &unknown, Renames::none(), history),
        ["anchor `lib/gone.rs::Book` of `d` is stale: nothing is at its path"]
    );
}

/// The role a stale anchor's code had is that of its definition where its
/// file is judged: a helper under `#[cfg(test)] mod tests` of a file of
/// code is test code, and is repaired to the test module it moved into.
#[test]
fn a_test_helper_follows_its_code_into_another_test_module() {
    let tests = "#[cfg(test)]\nmod tests { fn fixture() {} }\n";
    let here = facts(
        &[
            MANIFEST,
            ("src/a.rs", "pub fn a() {}\n"),
            ("src/b.rs", &format!("pub fn b() {{}}\n{tests}")),
        ],
        &[],
    );
    let decisions = [(
        "d",
        decision(&["src/a.rs::tests::fixture"], StatusName::Accepted),
    )];
    let mut history = prior(&[
        ("src/a.rs", &format!("pub fn a() {{}}\n{tests}")),
        ("src/b.rs", "pub fn b() {}\n"),
    ]);
    let landed = format!("pub fn b() {{}}\n{tests}");
    history.extend([held(LOST, "src/b.rs", &landed)]);

    arrived(&mut history, LOST, &["src/b.rs"]);
    let lines = repaired(&decisions, &here, Renames::none(), history);

    assert_eq!(
        lines,
        [
            "anchor `src/a.rs::tests::fixture` of `d` is stale: its file defines nothing its symbol names; repair: `src/b.rs::tests::fixture`"
        ]
    );
}

/// Only a stale anchor is repaired: a planned one names what is not
/// written yet, a pending one's code is on a branch, and with git not
/// asked no anchor is known stale. A proposed revision of an accepted
/// decision is stale, so repaired.
#[test]
fn a_planned_or_pending_anchor_is_never_repaired() {
    use crate::store::AcceptedText;

    let here = facts(
        &[
            MANIFEST,
            ("src/lib.rs", ""),
            ("src/moved.rs", "pub fn gone() {}\n"),
        ],
        &[],
    );
    let elsewhere = [Elsewhere {
        worktree: Worktree {
            path: "/w/branch".into(),
            branch: Some("branch".into()),
        },
        facts: facts(&[("src/lib.rs", "pub fn branch() {}\n")], &[]),
        fork: None,
    }];
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
            decision(&["src/lib.rs::gone"], StatusName::Proposed),
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
    for tree in elsewhere {
        asked.read(tree);
    }
    let mut history = prior(&[("src/lib.rs", "pub fn gone() {}\npub fn branch() {}\n")]);
    let moved = "pub fn gone() {}\n";
    history.extend([held(LOST, "src/moved.rs", moved)]);
    arrived(&mut history, LOST, &["src/moved.rs"]);
    let asked = asked_with_repairs(asked, &here, &Renames::none(), &history);

    let found = findings(&named, &here, &asked);

    let repairs: Vec<(&str, Option<String>)> = found
        .iter()
        .map(|finding| {
            let repair = match &finding.kind {
                Kind::Missing(_, Checked::Stale { repair, .. }) => repair.as_ref(),
                Kind::Missing(_, Checked::Pending(_) | Checked::Planned)
                | Kind::Malformed(_)
                | Kind::Unverified(_)
                | Kind::Ambiguous(_)
                | Kind::Weak(_) => None,
            };
            (finding.decision, repair.map(ToString::to_string))
        })
        .collect();
    assert_eq!(
        repairs,
        [
            ("revised", Some("src/moved.rs::gone".to_owned())),
            ("pending", None),
            ("planned", None),
        ]
    );
}

/// A file git reports renamed repairs the anchors on it when the rename
/// carried their code: a symbol anchor whose definitions before the rename
/// have the span hash they have in the file the rename made, however the
/// file was edited since, the new path read from the renamed paths' facts
/// when the tree's lack it. A symbol the rename edited follows it not; one
/// that left the file in the rename is searched for where it went. The file
/// anchor does not follow, since symbols that resolved in the old file are
/// missing from the file the rename made.
#[test]
fn a_file_git_reports_renamed_repairs_its_anchors() {
    let here = facts(
        &[
            MANIFEST,
            ("src/new.rs", "pub fn kept() { 3 }\npub fn edited() { 2 }\n"),
            ("src/other.rs", "pub fn left() {}\n"),
        ],
        &[],
    );
    let renames = Renames {
        to: BTreeMap::from([
            ("src/old.rs".to_owned(), renamed("src/new.rs", false)),
            ("conf/old.toml".to_owned(), renamed("conf/new.toml", false)),
        ]),
        facts: facts(&[("conf/new.toml", "[profile]\npanic = 1\n")], &[]),
    };
    let mut history = prior(&[
        (
            "src/old.rs",
            "pub fn kept() {}\npub fn edited() { 1 }\npub fn left() {}\npub fn lost() {}\n",
        ),
        ("conf/old.toml", "[profile]\npanic = 1\n"),
    ]);
    history.extend([
        held(
            LOST,
            "src/new.rs",
            "pub fn kept() {}\npub fn edited() { 2 }\n",
        ),
        held(LOST, "conf/new.toml", "[profile]\npanic = 1\n"),
        held(LOST, "src/other.rs", "pub fn left() {}\n"),
    ]);
    let anchors = [
        "src/old.rs",
        "src/old.rs::kept",
        "src/old.rs::edited",
        "src/old.rs::left",
        "src/old.rs::lost",
        "conf/old.toml::profile::panic",
        "src/never.rs",
    ];
    let decisions = [("d", decision(&anchors, StatusName::Accepted))];

    arrived(&mut history, LOST, &["src/other.rs"]);
    let lines = repaired(&decisions, &here, renames, history);

    let gone = |at: &str, repair: Option<&str>| {
        let fix = repair.map_or(String::new(), |to| format!("; repair: `{to}`"));
        format!("anchor `{at}` of `d` is stale: nothing is at its path{fix}")
    };
    assert_eq!(
        lines,
        [
            gone(
                "conf/old.toml::profile::panic",
                Some("conf/new.toml::profile::panic")
            ),
            gone("src/never.rs", None),
            gone("src/old.rs", None),
            gone("src/old.rs::edited", None),
            gone("src/old.rs::kept", Some("src/new.rs::kept")),
            gone("src/old.rs::left", Some("src/other.rs::left")),
            gone("src/old.rs::lost", None),
        ]
    );
}

/// A rename carries a symbol only to definitions of the kinds it named in
/// the file the rename made: a function a later commit split out of the
/// renamed file, leaving a `mod` declaration of its name, is searched for
/// in the renamed file's history and repaired where it went. The file
/// anchor follows the exact rename though a later edit dropped one of its
/// functions.
#[test]
fn a_function_split_out_after_an_exact_rename_is_repaired_where_it_went() {
    let here = facts(
        &[
            MANIFEST,
            ("src/new.rs", "mod join;\npub fn other() {}\n"),
            ("src/new/join.rs", "pub fn join() {}\n"),
        ],
        &[],
    );
    let renames = Renames {
        to: BTreeMap::from([("src/old.rs".to_owned(), renamed("src/new.rs", true))]),
        facts: facts(&[], &[]),
    };
    let text = "pub fn join() {}\npub fn other() {}\npub fn dropped() {}\n";
    let mut history = prior(&[("src/old.rs", text)]);
    history.extend([
        held(LOST, "src/new.rs", text),
        held(BEFORE_LATER, "src/new.rs", text),
        held(LATER, "src/new/join.rs", "pub fn join() {}\n"),
    ]);
    let commits = vec![LATER.to_owned(), LOST.to_owned()];
    history.changed("src/new.rs".to_owned(), commits, false);
    let anchors = [
        "src/old.rs",
        "src/old.rs::join",
        "src/old.rs::other",
        "src/old.rs::dropped",
    ];
    let decisions = [("d", decision(&anchors, StatusName::Accepted))];

    arrived(&mut history, LATER, &["src/new/join.rs"]);
    let lines = repaired(&decisions, &here, renames, history);

    let gone = |at: &str, repair: &str| {
        format!("anchor `{at}` of `d` is stale: nothing is at its path{repair}")
    };
    assert_eq!(
        lines,
        [
            gone("src/old.rs", "; repair: `src/new.rs`"),
            gone("src/old.rs::dropped", ""),
            gone("src/old.rs::join", "; repair: `src/new/join.rs::join`"),
            gone("src/old.rs::other", "; repair: `src/new.rs::other`"),
        ]
    );
}

/// git pairs two files by similarity alone, so a rename carries a file
/// anchor only when it is shown to: exact into a file that defines at least
/// one item, or followed by a symbol anchor whose code the rename kept,
/// while, for an inexact rename, no symbol that resolved there before is
/// missing from the file it made. A header-only or an empty file has no
/// identity to carry, a `main` the rename edited is no evidence, and one
/// boilerplate `main` carried unchanged does not carry a file whose own
/// function the new file lacks.
#[test]
fn a_file_anchor_follows_only_a_verified_rename() {
    let files = [
        ("src/beta.rs", "// License.\n"),
        ("src/empty.rs", ""),
        ("src/bin/report.rs", "fn main() { report() }\n"),
        ("src/same.rs", "pub fn same() {}\n"),
        (
            "src/bin/export.rs",
            "fn main() { run() }\nfn export_csv() {}\n",
        ),
        ("src/kept.rs", "pub fn kept() {}\npub fn added() {}\n"),
    ];
    let mut tree = vec![MANIFEST];
    tree.extend(files);
    let here = facts(&tree, &[]);
    let renames = Renames {
        to: BTreeMap::from([
            ("src/alpha.rs".to_owned(), renamed("src/beta.rs", true)),
            ("src/void.rs".to_owned(), renamed("src/empty.rs", true)),
            (
                "src/bin/import.rs".to_owned(),
                renamed("src/bin/report.rs", false),
            ),
            ("src/copy.rs".to_owned(), renamed("src/same.rs", true)),
            (
                "src/bin/load.rs".to_owned(),
                renamed("src/bin/export.rs", false),
            ),
            ("src/keep.rs".to_owned(), renamed("src/kept.rs", false)),
        ]),
        facts: facts(&[], &[]),
    };
    let mut history = prior(&[
        ("src/bin/import.rs", "fn main() { import() }\n"),
        (
            "src/bin/load.rs",
            "fn main() { run() }\nfn import_csv() {}\n",
        ),
        ("src/keep.rs", "pub fn kept() {}\n"),
        ("src/copy.rs", "pub fn same() {}\n"),
    ]);
    history.extend(files.iter().map(|(path, text)| held(LOST, path, text)));
    let anchors = [
        "src/alpha.rs",
        "src/void.rs",
        "src/bin/import.rs",
        "src/bin/import.rs::main",
        "src/copy.rs",
        "src/bin/load.rs",
        "src/bin/load.rs::main",
        "src/bin/load.rs::import_csv",
        "src/keep.rs",
        "src/keep.rs::kept",
    ];
    let decisions = [("cfg", decision(&anchors, StatusName::Accepted))];

    let lines = repaired(&decisions, &here, renames, history);

    let gone = |at: &str, repair: &str| {
        format!("anchor `{at}` of `cfg` is stale: nothing is at its path{repair}")
    };
    assert_eq!(
        lines,
        [
            gone("src/alpha.rs", ""),
            gone("src/bin/import.rs", ""),
            gone("src/bin/import.rs::main", ""),
            gone("src/bin/load.rs", ""),
            gone("src/bin/load.rs::import_csv", ""),
            gone(
                "src/bin/load.rs::main",
                "; repair: `src/bin/export.rs::main`"
            ),
            gone("src/copy.rs", "; repair: `src/same.rs`"),
            gone("src/keep.rs", "; repair: `src/kept.rs`"),
            gone("src/keep.rs::kept", "; repair: `src/kept.rs::kept`"),
            gone("src/void.rs", ""),
        ]
    );
}

/// A move not committed is judged in the work tree: the anchor's file
/// wrote the item at `HEAD`, and a file that did not then holds it now
/// with the same span hash.
#[test]
fn a_move_not_committed_is_repaired_from_the_work_tree() {
    let ported = "pub fn ported(n: u8) -> u8 { n + 1 }\n";
    let here = facts(
        &[
            MANIFEST,
            ("src/a.rs", "pub fn a() {}\n"),
            ("src/b.rs", &format!("pub fn b() {{}}\n{ported}")),
        ],
        &[],
    );
    let decisions = [("d", decision(&["src/a.rs::ported"], StatusName::Accepted))];
    let mut history = Prior::default();
    history.extend([
        held("HEAD", "src/a.rs", &format!("pub fn a() {{}}\n{ported}")),
        held("HEAD", "src/b.rs", "pub fn b() {}\n"),
    ]);
    history.changed("src/a.rs".to_owned(), vec![LOST.to_owned()], true);

    let lines = repaired(&decisions, &here, Renames::none(), history);

    assert_eq!(
        lines,
        [
            "anchor `src/a.rs::ported` of `d` is stale: its file defines nothing its symbol names; repair: `src/b.rs::ported`"
        ]
    );
}

/// The file a rename carried a symbol into is not searched while a macro
/// invocation there spells the name: the code may stand in it, which no
/// index sees, so a namesake that took the same code elsewhere is no move.
#[test]
fn a_renamed_file_that_spells_the_name_in_an_invocation_is_not_searched() {
    let here = facts(
        &[
            MANIFEST,
            ("src/new.rs", "wrap!(join);\npub fn other() {}\n"),
            ("src/b.rs", "pub fn join() {}\n"),
        ],
        &[],
    );
    let renames = Renames {
        to: BTreeMap::from([("src/old.rs".to_owned(), renamed("src/new.rs", true))]),
        facts: facts(&[], &[]),
    };
    let text = "pub fn join() {}\npub fn other() {}\n";
    let mut history = prior(&[("src/old.rs", text)]);
    history.extend([
        held(LOST, "src/new.rs", text),
        held(BEFORE_LATER, "src/new.rs", text),
        held(BEFORE_LATER, "src/b.rs", ""),
        held(LATER, "src/b.rs", "pub fn join() {}\n"),
    ]);
    let commits = vec![LATER.to_owned(), LOST.to_owned()];
    history.changed("src/new.rs".to_owned(), commits, false);
    let decisions = [("d", decision(&["src/old.rs::join"], StatusName::Accepted))];

    arrived(&mut history, LATER, &["src/b.rs"]);
    let lines = repaired(&decisions, &here, renames, history);

    assert_eq!(
        lines,
        ["anchor `src/old.rs::join` of `d` is stale: nothing is at its path"]
    );
}
