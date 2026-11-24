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
                index: index(kind, text.as_bytes()).map_err(FileFault::Index),
            };
            ((*path).to_owned(), file)
        })
        .collect();
    entries.extend(
        other
            .iter()
            .map(|(path, entry)| ((*path).to_owned(), entry.clone())),
    );
    CodeFacts::new(entries, &walked)
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
    elsewhere: Option<&[Elsewhere]>,
) -> Vec<String> {
    let named: Vec<(&str, &Decision)> = decisions.iter().map(|(n, d)| (*n, d)).collect();
    let here = Here::of(&named, here);
    let Some(elsewhere) = elsewhere else {
        return unasked(&named, &here)
            .iter()
            .map(ToString::to_string)
            .collect();
    };
    let unresolved = here.unresolved(&named);
    let asked = Asked::of(&unresolved, elsewhere);
    findings(&named, &here, &asked)
        .iter()
        .map(ToString::to_string)
        .collect()
}

/// No other worktree: git asked, and it named none.
const ASKED: Option<&[Elsewhere]> = Some(&[]);

const LIB: &str = "pub struct Book { pages: u32 }\nimpl Book { fn pages(&self) -> u32 { self.pages } }\npub fn open() {}\n";

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
    let here = facts(&[("src/lib.rs", LIB)], &[]);
    let anchors = ["gone.rs", "src/lib.rs::close", "src/lib.rs::pages"];
    let decisions = [
        ("kept", decision(&anchors, StatusName::Accepted)),
        ("planned", decision(&anchors, StatusName::Proposed)),
    ];
    let holder = Elsewhere {
        worktree: Worktree {
            path: PathBuf::from("/w/feature"),
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
            "anchor `src/lib.rs::pages` of `kept` is mixed: its symbol names 2 definitions of more than one kind",
            "anchor `src/lib.rs::pages` of `planned` is mixed: its symbol names 2 definitions of more than one kind",
        ]
    );
    let asked = judged(&decisions, &here, Some(&[holder]));
    let kinds: Vec<&str> = asked
        .iter()
        .map(|line| line.split(": ").next().unwrap())
        .collect();
    assert_eq!(
        kinds,
        [
            "anchor `gone.rs` of `kept` is stale",
            "anchor `src/lib.rs::pages` of `kept` is mixed",
            "anchor `src/lib.rs::pages` of `planned` is mixed",
            "anchor `src/lib.rs::close` of `kept` is pending",
            "anchor `gone.rs` of `planned` is planned",
            "anchor `src/lib.rs::close` of `planned` is planned",
        ]
    );
}

/// An anchor of a decision ever accepted that names nothing here and
/// names what another worktree holds is pending on the first such
/// worktree, a definition it names twice there included; one of a
/// decision never accepted is planned, wherever its code is.
#[test]
fn an_anchor_another_worktree_holds_is_pending_on_its_branch() {
    let here = facts(&[("src/lib.rs", LIB)], &[]);
    let tree = |path: &str, branch: Option<&str>, files: &[(&str, &str)]| Elsewhere {
        worktree: Worktree {
            path: PathBuf::from(path),
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
                    "pub fn close() {}\npub struct X;\nimpl X { fn twice(&self) {} }\nimpl X { fn twice(&self) {} }\n",
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
        judged(&decisions, &here, Some(&elsewhere)),
        [
            format!("anchor `src/lib.rs::close` of `accepted` is pending: {missing}; it resolves in the worktree at /w/feature on branch `feature`"),
            format!("anchor `src/lib.rs::late` of `accepted` is pending: {missing}; it resolves in the worktree at /w/detached, whose HEAD is detached"),
            format!("anchor `src/lib.rs::twice` of `accepted` is pending: {missing}; it resolves in the worktree at /w/feature on branch `feature`"),
            "anchor `src/new.rs` of `accepted` is pending: nothing is at its path here; it resolves in the worktree at /w/feature on branch `feature`".to_owned(),
            format!("anchor `src/lib.rs::late` of `proposed` is planned: {planned}"),
            format!("anchor `src/lib.rs::never` of `proposed` is planned: {planned}"),
        ]
    );
}

/// A symbol that names definitions of several paths is ambiguous with
/// their count; one that names definitions of one path and more than one
/// kind, a field and its getter, is mixed with theirs.
#[test]
fn several_paths_are_ambiguous_and_one_path_of_several_kinds_mixed() {
    let shelf = "pub struct Shelf { pages: u32 }\n";
    let here = facts(&[("src/lib.rs", &format!("{LIB}{shelf}"))], &[]);
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
        [
            "anchor `src/lib.rs::pages` of `d` is ambiguous: its symbol names definitions of 2 paths",
            "anchor `src/lib.rs::Book::pages` of `d` is mixed: its symbol names 2 definitions of more than one kind",
        ]
    );
}

/// A resolved symbol of one plain name is weak when [`WEAK_FILES`] or more
/// walked files of its kind hold a definition of it at any depth, a type
/// a macro invocation defines included: a qualified symbol, a file anchor
/// and a kind that is no code never are.
#[test]
fn one_name_that_ten_files_of_its_kind_define_is_weak() {
    let mut files: Vec<(String, String)> = (0..WEAK_FILES - 1)
        .map(|n| {
            (
                format!("src/m{n}.rs"),
                "mod inner { struct S { run: u8 } }\n".to_owned(),
            )
        })
        .collect();
    files.push(("src/lib.rs".to_owned(), "pub fn run() {}\n".to_owned()));
    let maker = "macro_rules! make { ($name:ident) => { pub struct $name; }; }\n";
    for n in 0..WEAK_FILES {
        files.push((format!("src/made{n}.rs"), format!("{maker}make!(T{n});\n")));
    }
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
                "src/m0.rs::S::run",
                "src/lib.rs",
                "docs/d0.md::run",
                "src/made0.rs::T0",
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
    let here = facts(&[("src/lib.rs", LIB)], &[]);
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
            "anchor `src/lib.rs::pages` of `a` is mixed",
            "anchor `src/lib.rs::pages` of `b` is mixed",
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
        path: PathBuf::from("/w"),
        branch: None,
    };
    let kinds: [Kind<Checked>; 7] = [
        Kind::Missing(missing(), Checked::Stale { unexpanded: None }),
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
    files.push(("src/lib.rs".to_owned(), "pub fn run() {}\n".to_owned()));
    let walked: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    let hidden = Entry::File {
        kind: FileKind::Rust,
        index: index(FileKind::Rust, b"pub fn run() {}\n").map_err(FileFault::Index),
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

/// A file read in another worktree is expanded with the calling tree's
/// macros, which its walk holds: a type a macro of another file defines
/// there is pending. When the file there invokes a macro this tree does
/// not define, spelling the anchor's name, the stale finding says so.
#[test]
fn another_worktrees_file_is_expanded_with_the_calling_trees_macros() {
    let maker = "macro_rules! define { ($n:ident) => { pub struct $n; }; }\n";
    let here = facts(
        &[
            ("Cargo.toml", "[package]\nname = \"p\"\n"),
            ("src/macros.rs", maker),
            ("src/lib.rs", ""),
        ],
        &[],
    );
    let there = |text: &str| {
        let entry = Entry::File {
            kind: FileKind::Rust,
            index: index(FileKind::Rust, text.as_bytes()).map_err(FileFault::Index),
        };
        let read = BTreeMap::from([("src/lib.rs".to_owned(), Some(entry))]);
        Elsewhere {
            worktree: Worktree {
                path: PathBuf::from("/w/feature"),
                branch: Some("feature".to_owned()),
            },
            facts: here.beside(&read),
        }
    };
    let decisions = [("d", decision(&["src/lib.rs::Foo"], StatusName::Accepted))];

    let defined = judged(&decisions, &here, Some(&[there("define!(Foo);\n")]));
    let unknown = judged(&decisions, &here, Some(&[there("elsewhere!(Foo);\n")]));

    let missing = "its file defines nothing its symbol names";
    assert_eq!(
        defined,
        [format!(
            "anchor `src/lib.rs::Foo` of `d` is pending: {missing} here; it resolves in the \
             worktree at /w/feature on branch `feature`"
        )]
    );
    assert_eq!(
        unknown,
        [format!(
            "anchor `src/lib.rs::Foo` of `d` is stale: {missing}; its file in the worktree at \
             /w/feature spells it in a macro invocation this tree's macros do not expand, so \
             it may be pending there"
        )]
    );
}
