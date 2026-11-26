use std::collections::{BTreeMap, BTreeSet};

use super::*;
use crate::code::{Entry, FileFault, FileKind, ReadFault, Resolution, index};
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
                index: index(kind, text.as_bytes(), &crate::code::BlankMacros::default())
                    .map_err(FileFault::Index),
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
        index: index(
            FileKind::Rust,
            b"pub fn run() {}\n",
            &crate::code::BlankMacros::default(),
        )
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
    let searches = repair::searched(&asked, here, renames, &repairs);
    let gained = repair::gained(searches, prior);
    repairs.extend(repair::moved(&gained, here.facts, prior));
    asked.with_repairs(repairs)
}

/// The commit every anchored file of these tests last changed in, so lost
/// its code in, and its parent, where each is judged.
const LOST: &str = "X";
const BEFORE: &str = "X^";

/// A later commit, and its parent.
const LATER: &str = "Y";
const BEFORE_LATER: &str = "Y^";

/// What `path` held at `revision`, `text`, as git's history gives it.
fn held(revision: &str, path: &str, text: &str) -> (String, String, Entry) {
    let kind = FileKind::of(path).unwrap();
    let index = index(kind, text.as_bytes(), &crate::code::BlankMacros::default())
        .map_err(FileFault::Index);
    (
        revision.to_owned(),
        path.to_owned(),
        Entry::File { kind, index },
    )
}

/// What git's history held at [`BEFORE`], each of `files` a path and its
/// text, every one judged there, as a file last changed in [`LOST`].
fn prior(files: &[(&str, &str)]) -> Prior {
    let mut prior = Prior::default();
    prior.extend(files.iter().map(|(path, text)| held(BEFORE, path, text)));
    for (path, _) in files {
        prior.judge_at((*path).to_owned(), Judged::Before(LOST.to_owned()));
    }
    prior
}

/// `prior` with `target` gaining, in `commit`, the symbols the anchored
/// `file` lost: `commit` is the one commit since `file` was judged that
/// changed how often `target` spells each name it holds in `text`, which
/// it held there, and nothing at its parent.
fn gained_in(prior: &mut Prior, commit: &str, file: &str, target: &str, text: &str) {
    for name in names(text) {
        prior.spelled(file, target, name, vec![commit.to_owned()]);
    }
    prior.extend([held(commit, target, text)]);
}

/// Each identifier `text` spells.
fn names(text: &str) -> BTreeSet<&str> {
    text.split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|word| !word.is_empty())
        .collect()
}

/// The manifest of a package at the root of the tree.
const MANIFEST: (&str, &str) = ("Cargo.toml", "[package]\nname = \"p\"\n");

const BOOK: &str = "pub struct Book;\nimpl Book { fn pages() {} }\n";

/// A clock whose `tick` is a method: its whole path is `Clock::tick`.
const CLOCK: &str = "pub struct Clock;\nimpl Clock { fn tick() {} }\n";

/// A stale symbol anchor is repaired to the one other file of its target
/// that defines its symbol as its whole path when git's history shows the
/// move, the file having gained it in the commit the anchor's file lost
/// it: a method named alone in that file before, not its whole path, is
/// no namesake. Not when two files define it, nor when it moved and was
/// renamed, nor on a file of another language, nor for any other cause.
#[test]
fn a_symbol_that_moved_to_one_other_file_is_repaired_there() {
    let here = facts(
        &[
            MANIFEST,
            ("src/lib.rs", "pub fn stay() {}\n"),
            ("src/moved.rs", BOOK),
            ("src/clock.rs", &format!("{CLOCK}pub fn tick() {{}}\n")),
            ("src/twice_a.rs", "pub fn twice() {}\n"),
            ("src/twice_b.rs", "pub fn twice() {}\n"),
            ("src/moved.py", "def run():\n    pass\n"),
        ],
        &[("notes.txt", Entry::Unindexed)],
    );
    let anchors = [
        "src/lib.rs::Book::pages",
        "src/lib.rs::tick",
        "src/gone.rs::Book",
        "src/lib.rs::twice",
        "src/gone.py::run",
        "src/lib.rs::renamed_too",
        "notes.txt::line",
        "docs.md::Install",
    ];
    let decisions = [("d", decision(&anchors, StatusName::Accepted))];
    let history = || {
        let before = format!("pub fn stay() {{}}\npub fn tick() {{}}\n{BOOK}");
        let mut prior = prior(&[
            ("src/lib.rs", &before),
            ("src/gone.rs", BOOK),
            ("src/gone.py", "def run():\n    pass\n"),
            ("src/clock.rs", CLOCK),
        ]);
        gained_in(&mut prior, LOST, "src/lib.rs", "src/moved.rs", BOOK);
        gained_in(&mut prior, LOST, "src/gone.rs", "src/moved.rs", BOOK);
        let ticks = format!("{CLOCK}pub fn tick() {{}}\n");
        gained_in(&mut prior, LOST, "src/lib.rs", "src/clock.rs", &ticks);
        prior
    };

    let lines = repaired(&decisions, &here, Renames::none(), history());
    let unshown = repaired(&decisions, &here, Renames::none(), Prior::default());

    let stale = |at: &str, cause: &str| format!("anchor `{at}` of `d` is stale: {cause}");
    let missing = "its file defines nothing its symbol names";
    let stales = |book: &str, pages: &str, tick: &str| {
        vec![
            stale("docs.md::Install", "nothing is at its path"),
            stale("src/gone.py::run", "nothing is at its path"),
            stale(
                "src/gone.rs::Book",
                &format!("nothing is at its path{book}"),
            ),
            stale("src/lib.rs::Book::pages", &format!("{missing}{pages}")),
            stale("src/lib.rs::renamed_too", missing),
            stale("src/lib.rs::tick", &format!("{missing}{tick}")),
            stale("src/lib.rs::twice", missing),
        ]
    };
    let mut expected = stales(
        "; repair: `src/moved.rs::Book`",
        "; repair: `src/moved.rs::Book::pages`",
        "; repair: `src/clock.rs::tick`",
    );
    let unverified = "anchor `notes.txt::line` of `d` is unverified: its symbol is on a kind of file no resolver reads, so what it names is not verified";
    expected.push(unverified.to_owned());
    assert_eq!(lines, expected);
    let mut expected = stales("", "", "");
    expected.push(unverified.to_owned());
    assert_eq!(unshown, expected, "no history shows no move");
}

/// A namesake is no move: the search matches the symbol as a whole path,
/// so a bare name never lands on a type's method or a test module's
/// helper; it gives no repair to a file that defined the symbol where the
/// anchor's own file last held it, the two having stood side by side; nor
/// to one that gained it in a later commit no carrier links to the loss,
/// a namesake written after the code was deleted (a function dropped from
/// its file, a file deleted, a type of the library and a later one of the
/// binary); nor to one that gained it in the loss, lost it later and holds
/// it again, written anew; nor for a symbol the anchor's file never
/// defined.
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
            ("src/net.rs", "pub fn retry_write() {}\n"),
            ("src/http.rs", "pub fn evict() {}\n"),
            ("src/lib.rs", "pub fn open() {}\n"),
            ("src/main.rs", "struct Config;\nfn main() {}\n"),
            ("src/phantom.rs", "pub fn phantom() {}\n"),
            ("src/queue.rs", "pub fn push() {}\n"),
            ("src/pool.rs", "pub fn drain() {}\n"),
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
        "src/queue.rs::drain",
    ];
    let decisions = [("d", decision(&anchors, StatusName::Accepted))];
    let mut history = prior(&[
        ("src/cli.rs", "pub fn run() {}\npub fn waits_on() {}\n"),
        ("src/http_old.rs", "pub fn parse_header() {}\n"),
        ("src/mail.rs", "pub fn parse_header() {}\n"),
        (
            "src/store.rs",
            "pub fn write() {}\npub fn retry_write() {}\n",
        ),
        ("src/cache.rs", "pub fn evict() {}\n"),
        ("src/lib.rs", "pub fn open() {}\npub struct Config;\n"),
        ("src/queue.rs", "pub fn push() {}\npub fn drain() {}\n"),
    ]);
    let later = [
        ("src/store.rs", "src/net.rs", "pub fn retry_write() {}\n"),
        ("src/cache.rs", "src/http.rs", "pub fn evict() {}\n"),
        (
            "src/lib.rs",
            "src/main.rs",
            "struct Config;\nfn main() {}\n",
        ),
    ];
    for (file, target, text) in later {
        gained_in(&mut history, LATER, file, target, text);
        history.changed(LOST.to_owned(), BTreeSet::from([file.to_owned()]));
    }
    let added: BTreeSet<String> = later.iter().map(|(_, t, _)| (*t).to_owned()).collect();
    history.changed(LATER.to_owned(), added);
    let phantom = "pub fn phantom() {}\n";
    gained_in(&mut history, LOST, "src/cli.rs", "src/phantom.rs", phantom);
    let drain = "pub fn drain() {}\n";
    let commits = vec![LATER.to_owned(), LOST.to_owned()];
    history.spelled("src/queue.rs", "src/pool.rs", "drain", commits);
    history.extend([
        held(LATER, "src/pool.rs", ""),
        held(BEFORE_LATER, "src/pool.rs", drain),
        held(LOST, "src/pool.rs", drain),
    ]);

    let lines = repaired(&decisions, &here, Renames::none(), history);

    assert!(
        lines.iter().all(|line| !line.contains("repair")),
        "{lines:#?}"
    );
    assert_eq!(lines.len(), anchors.len(), "{lines:#?}");
}

/// Code that moved in two steps, out of the anchor's file into another
/// file and from there into the one that holds it now, is repaired there:
/// the carrier gained it in the commit the anchor's file lost it and held
/// it before the last move. A file both commits changed that held the
/// symbol beside the code already, gained it only later, or named it in a
/// re-export alone, carried nothing.
#[test]
fn a_symbol_carried_through_another_file_is_repaired_where_it_landed() {
    let landed = "pub fn helper() {}\npub fn twin() {}\npub fn late() {}\npub fn shown() {}\n";
    let here = facts(
        &[
            MANIFEST,
            ("src/old.rs", "pub fn stay() {}\n"),
            ("src/mid.rs", "pub fn other() {}\n"),
            ("src/new.rs", landed),
        ],
        &[],
    );
    let anchors = [
        "src/old.rs::helper",
        "src/old.rs::twin",
        "src/old.rs::late",
        "src/old.rs::shown",
    ];
    let decisions = [("d", decision(&anchors, StatusName::Accepted))];
    let mut history = prior(&[(
        "src/old.rs",
        "pub fn stay() {}\npub fn helper() {}\npub fn twin() {}\npub fn late() {}\npub fn shown() {}\n",
    )]);
    let carried = "pub fn helper() {}\npub fn twin() {}\npub use crate::shown::shown;\n";
    history.extend([
        held(BEFORE, "src/mid.rs", "pub fn twin() {}\n"),
        held(LOST, "src/mid.rs", carried),
        held(
            BEFORE_LATER,
            "src/mid.rs",
            &format!("{carried}pub fn late() {{}}\n"),
        ),
    ]);
    gained_in(&mut history, LATER, "src/old.rs", "src/new.rs", landed);
    let both = |paths: &[&str]| paths.iter().map(|path| (*path).to_owned()).collect();
    history.changed(LOST.to_owned(), both(&["src/old.rs", "src/mid.rs"]));
    history.changed(LATER.to_owned(), both(&["src/mid.rs", "src/new.rs"]));

    let lines = repaired(&decisions, &here, Renames::none(), history);

    let stale = |at: &str, repair: &str| {
        format!("anchor `{at}` of `d` is stale: its file defines nothing its symbol names{repair}")
    };
    assert_eq!(
        lines,
        [
            stale("src/old.rs::helper", "; repair: `src/new.rs::helper`"),
            stale("src/old.rs::late", ""),
            stale("src/old.rs::shown", ""),
            stale("src/old.rs::twin", ""),
        ]
    );
}

/// Two symbols of one file that went to one candidate are each judged by
/// the commits that changed the candidate's spelling of their own name:
/// one it gained in the commit the file lost both is repaired, one it
/// gained later from no carrier is not.
#[test]
fn each_name_is_judged_by_the_commits_that_changed_its_own_spelling() {
    let both = "pub fn alpha() {}\npub fn beta() {}\n";
    let here = facts(
        &[
            MANIFEST,
            ("src/old.rs", "pub fn stay() {}\n"),
            ("src/new.rs", both),
        ],
        &[],
    );
    let anchors = ["src/old.rs::alpha", "src/old.rs::beta"];
    let decisions = [("d", decision(&anchors, StatusName::Accepted))];
    let mut history = prior(&[(
        "src/old.rs",
        "pub fn stay() {}\npub fn alpha() {}\npub fn beta() {}\n",
    )]);
    history.spelled("src/old.rs", "src/new.rs", "alpha", vec![LOST.to_owned()]);
    history.spelled("src/old.rs", "src/new.rs", "beta", vec![LATER.to_owned()]);
    history.extend([
        held(LOST, "src/new.rs", "pub fn alpha() {}\n"),
        held(BEFORE_LATER, "src/new.rs", "pub fn alpha() {}\n"),
        held(LATER, "src/new.rs", both),
    ]);

    let lines = repaired(&decisions, &here, Renames::none(), history);

    let missing = "its file defines nothing its symbol names";
    assert_eq!(
        lines,
        [
            format!(
                "anchor `src/old.rs::alpha` of `d` is stale: {missing}; repair: `src/new.rs::alpha`"
            ),
            format!("anchor `src/old.rs::beta` of `d` is stale: {missing}"),
        ]
    );
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
            "pub fn run() {}\nfn usage() {}\nfn sample() {}\npub struct Widget;\npub struct Engine;\npub struct Meters;\n",
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
    let moves = [
        ("src/spelled.rs", "src/gadget.rs", "pub struct Gadget;\n"),
        (
            "src/cli.rs",
            "src/fixtures.rs",
            "#[cfg(test)]\npub fn sample() {}\n",
        ),
        ("src/a/scale.rs", "src/b/mod.rs", "mod scale;\n"),
        ("src/cli.rs", "src/units.rs", "pub type Meters = u32;\n"),
    ];
    for (file, target, text) in moves {
        gained_in(&mut history, LOST, file, target, text);
    }

    let lines = repaired(&decisions, &here, Renames::none(), history);

    assert!(
        lines.iter().all(|line| !line.contains("repair")),
        "{lines:#?}"
    );
    assert_eq!(lines.len(), anchors.len(), "{lines:#?}");
    let unknown = facts(&[("lib/moved.rs", BOOK)], &[]);
    let lone = [("d", decision(&["lib/gone.rs::Book"], StatusName::Accepted))];
    let mut history = prior(&[("lib/gone.rs", BOOK)]);
    gained_in(&mut history, LOST, "lib/gone.rs", "lib/moved.rs", BOOK);
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
    gained_in(&mut history, LOST, "src/a.rs", "src/b.rs", &landed);

    let lines = repaired(&decisions, &here, Renames::none(), history);

    assert_eq!(
        lines,
        [
            "anchor `src/a.rs::tests::fixture` of `d` is stale: its file defines nothing its symbol names; repair: `src/b.rs::tests::fixture`"
        ]
    );
}

/// A file git reports renamed repairs the anchors on it when the rename is
/// shown to carry their code: a symbol anchor whose symbol's span hash at
/// the new path equals its hash at the old path before the rename, the new
/// path read from the renamed paths' facts when the tree's lack it; a
/// symbol the renamed file lacks is searched for as a moved one; one the
/// edit changed follows no inexact rename. The file anchor does not
/// follow, since symbols that resolved there before the rename do not
/// resolve at the new path.
#[test]
fn a_file_git_reports_renamed_repairs_its_anchors() {
    let here = facts(
        &[
            MANIFEST,
            ("src/new.rs", "pub fn kept() {}\npub fn edited() { 2 }\n"),
            ("src/other.rs", "pub fn left() {}\n"),
        ],
        &[],
    );
    let renamed = |to: &str| Renamed {
        to: to.to_owned(),
        exact: false,
        before: BEFORE.to_owned(),
    };
    let renames = Renames {
        to: BTreeMap::from([
            ("src/old.rs".to_owned(), renamed("src/new.rs")),
            ("conf/old.toml".to_owned(), renamed("conf/new.toml")),
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
    gained_in(
        &mut history,
        LOST,
        "src/old.rs",
        "src/other.rs",
        "pub fn left() {}\n",
    );
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

/// An exact rename carries a symbol only to definitions of the kinds it
/// named before: a function a later edit split out of the renamed file,
/// leaving a `mod` declaration of its name, is repaired where it went, the
/// renamed file having carried it there. The file anchor follows the exact
/// rename though a later edit dropped one of its functions.
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
        to: BTreeMap::from([(
            "src/old.rs".to_owned(),
            Renamed {
                to: "src/new.rs".to_owned(),
                exact: true,
                before: BEFORE.to_owned(),
            },
        )]),
        facts: facts(&[], &[]),
    };
    let text = "pub fn join() {}\npub fn other() {}\npub fn dropped() {}\n";
    let mut history = prior(&[("src/old.rs", text)]);
    history.extend([
        held(LOST, "src/new.rs", text),
        held(BEFORE_LATER, "src/new.rs", text),
    ]);
    gained_in(
        &mut history,
        LATER,
        "src/old.rs",
        "src/new/join.rs",
        "pub fn join() {}\n",
    );
    let paths = |names: &[&str]| names.iter().map(|name| (*name).to_owned()).collect();
    history.changed(LOST.to_owned(), paths(&["src/old.rs", "src/new.rs"]));
    history.changed(LATER.to_owned(), paths(&["src/new.rs", "src/new/join.rs"]));
    let anchors = [
        "src/old.rs",
        "src/old.rs::join",
        "src/old.rs::other",
        "src/old.rs::dropped",
    ];
    let decisions = [("d", decision(&anchors, StatusName::Accepted))];

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
/// one item, or followed by a symbol anchor whose code is unchanged while
/// no symbol that resolved there before is missing at the new path. A
/// header-only or an empty file has no identity to carry, a `main` the
/// rename edited is no evidence, and one boilerplate `main` carried
/// unchanged does not carry a file whose own function the new file lacks.
#[test]
fn a_file_anchor_follows_only_a_verified_rename() {
    let here = facts(
        &[
            MANIFEST,
            ("src/beta.rs", "// License.\n"),
            ("src/empty.rs", ""),
            ("src/bin/report.rs", "fn main() { report() }\n"),
            ("src/same.rs", "pub fn same() {}\n"),
            (
                "src/bin/export.rs",
                "fn main() { run() }\nfn export_csv() {}\n",
            ),
            ("src/kept.rs", "pub fn kept() {}\npub fn added() {}\n"),
        ],
        &[],
    );
    let renamed = |to: &str, exact| Renamed {
        to: to.to_owned(),
        exact,
        before: BEFORE.to_owned(),
    };
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
    let history = prior(&[
        ("src/bin/import.rs", "fn main() { import() }\n"),
        (
            "src/bin/load.rs",
            "fn main() { run() }\nfn import_csv() {}\n",
        ),
        ("src/keep.rs", "pub fn kept() {}\n"),
    ]);
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
    gained_in(&mut history, LOST, "src/lib.rs", "src/moved.rs", moved);
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
