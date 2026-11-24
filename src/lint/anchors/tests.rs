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
        Kind::Missing(
            missing(),
            Checked::Stale {
                unexpanded: None,
                repair: None,
            },
        ),
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

/// The findings of `decisions` with `renames` and what git's history held,
/// `prior`, as `check` prints them, no other worktree holding any anchor.
fn repaired(
    decisions: &[(&str, Decision)],
    here: &CodeFacts,
    renames: Renames,
    prior: Prior,
) -> Vec<String> {
    let named: Vec<(&str, &Decision)> = decisions.iter().map(|(n, d)| (*n, d)).collect();
    let here = Here::of(&named, here);
    let unresolved = here.unresolved(&named);
    let asked = Asked::of(&unresolved, &[])
        .with_renames(renames)
        .with_prior(prior);
    findings(&named, &here, &asked)
        .iter()
        .map(ToString::to_string)
        .collect()
}

/// What git's history held at revision `R`, each of `files` a path and its
/// text, every one judged at `R`.
fn prior(files: &[(&str, &str)]) -> Prior {
    let mut prior = Prior::default();
    for (path, text) in files {
        let kind = FileKind::of(path).unwrap();
        let index = index(kind, text.as_bytes()).map_err(FileFault::Index);
        prior.insert("R".into(), (*path).to_owned(), Entry::File { kind, index });
        prior.judge_at((*path).to_owned(), "R".into());
    }
    prior
}

/// The manifest of a package at the root of the tree.
const MANIFEST: (&str, &str) = ("Cargo.toml", "[package]\nname = \"p\"\n");

const BOOK: &str = "pub struct Book;\nimpl Book { fn pages() {} }\n";

/// A stale symbol anchor is repaired to the one other file of its crate
/// target that defines its symbol as its whole path when git's history
/// shows the move: not when two files define it, nor when it moved and was
/// renamed, nor on a file of another language, nor for any other cause.
#[test]
fn a_symbol_that_moved_to_one_other_file_is_repaired_there() {
    let here = facts(
        &[
            MANIFEST,
            ("src/lib.rs", "pub fn stay() {}\n"),
            ("src/moved.rs", BOOK),
            ("src/twice_a.rs", "pub fn twice() {}\n"),
            ("src/twice_b.rs", "pub fn twice() {}\n"),
            ("src/moved.py", "def run():\n    pass\n"),
        ],
        &[("notes.txt", Entry::Unindexed)],
    );
    let anchors = [
        "src/lib.rs::Book::pages",
        "src/gone.rs::Book",
        "src/lib.rs::twice",
        "src/gone.py::run",
        "src/lib.rs::renamed_too",
        "notes.txt::line",
        "docs.md::Install",
    ];
    let decisions = [("d", decision(&anchors, StatusName::Accepted))];
    let history = || {
        let before = "pub fn stay() {}\npub struct Book;\nimpl Book { fn pages() {} }\n";
        let python = "def run():\n    pass\n";
        prior(&[
            ("src/lib.rs", before),
            ("src/gone.rs", BOOK),
            ("src/gone.py", python),
        ])
    };

    let lines = repaired(&decisions, &here, Renames::none(), history());
    let unshown = repaired(&decisions, &here, Renames::none(), Prior::default());

    let stale = |at: &str, cause: &str| format!("anchor `{at}` of `d` is stale: {cause}");
    let missing = "its file defines nothing its symbol names";
    let stales = |book: &str, pages: &str| {
        vec![
            stale("docs.md::Install", "nothing is at its path"),
            stale("src/gone.py::run", "nothing is at its path"),
            stale(
                "src/gone.rs::Book",
                &format!("nothing is at its path{book}"),
            ),
            stale("src/lib.rs::Book::pages", &format!("{missing}{pages}")),
            stale("src/lib.rs::renamed_too", missing),
            stale("src/lib.rs::twice", missing),
        ]
    };
    let mut expected = stales(
        "; repair: `src/moved.rs::Book`",
        "; repair: `src/moved.rs::Book::pages`",
    );
    let unverified = "anchor `notes.txt::line` of `d` is unverified: its symbol is on a kind of file no resolver reads, so what it names is not verified";
    expected.push(unverified.to_owned());
    assert_eq!(lines, expected);
    let mut expected = stales("", "");
    expected.push(unverified.to_owned());
    assert_eq!(unshown, expected, "no history shows no move");
}

/// A namesake is no move: the search matches the symbol as a whole path,
/// so a bare name never lands on a type's method or a test module's
/// helper, and gives no repair to a file that defined the symbol at the
/// revision where the anchor's own file last held it, the two having stood
/// side by side.
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
        ],
        &[],
    );
    let anchors = [
        "src/cli.rs::run",
        "src/cli.rs::waits_on",
        "src/http.rs::parse_header",
    ];
    let decisions = [("d", decision(&anchors, StatusName::Accepted))];
    let history = prior(&[
        ("src/cli.rs", "pub fn run() {}\npub fn waits_on() {}\n"),
        ("src/http.rs", "pub fn parse_header() {}\n"),
        ("src/mail.rs", "pub fn parse_header() {}\n"),
    ]);

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
/// the index cannot expand, while another file re-exports it; the one file
/// that defines it is of another crate target (a test, a binary, a crate
/// of its own, vendored code with a manifest of its own), defines it as
/// test code for code, by a macro invocation or as a re-export; or no
/// manifest the walk took is above the anchor, so its crate is unknown.
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
            ("tests/cli.rs", "fn run(args: &[&str]) {}\n"),
            ("src/bin/tool.rs", "fn main() {}\nfn usage() {}\n"),
            ("src/fixtures.rs", "#[cfg(test)]\npub fn sample() {}\n"),
            ("src/api.rs", "pub use crate::inner::Engine;\n"),
            (
                "src/made.rs",
                "macro_rules! make { ($n:ident) => { pub struct $n; }; }\nmake!(Widget);\n",
            ),
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
        "crates/a/src/lib.rs::Config::new",
        "src/de.rs::Deserializer::deserialize_any",
        "src/cli.rs::usage",
        "src/cli.rs::sample",
        "src/cli.rs::Widget",
        "src/cli.rs::Engine",
    ];
    let decisions = [("d", decision(&anchors, StatusName::Accepted))];
    let history = prior(&[
        (
            "src/fbqc.rs",
            "pub trait Routing { fn route_cost(&self) -> u8; }\n",
        ),
        (
            "src/cli.rs",
            "pub fn run() {}\nfn usage() {}\nfn sample() {}\npub struct Widget;\npub struct Engine;\n",
        ),
        ("src/refusal.rs", "pub struct Report;\n"),
        (
            "crates/a/src/lib.rs",
            "pub struct Config;\nimpl Config { pub fn new() {} }\n",
        ),
        (
            "src/de.rs",
            "pub trait Deserializer { fn deserialize_any(self); }\n",
        ),
    ]);

    let lines = repaired(&decisions, &here, Renames::none(), history);

    assert!(
        lines.iter().all(|line| !line.contains("repair")),
        "{lines:#?}"
    );
    assert_eq!(lines.len(), anchors.len(), "{lines:#?}");
    let unknown = facts(&[("lib/moved.rs", BOOK)], &[]);
    let lone = [("d", decision(&["lib/gone.rs::Book"], StatusName::Accepted))];
    let history = prior(&[("lib/gone.rs", BOOK)]);
    assert_eq!(
        repaired(&lone, &unknown, Renames::none(), history),
        ["anchor `lib/gone.rs::Book` of `d` is stale: nothing is at its path"]
    );
}

/// A file git reports renamed repairs the anchors on it when the rename is
/// shown to carry their code: a symbol anchor whose symbol's span hash at
/// the new path equals its hash at the old path before the rename, the new
/// path read from the renamed paths' facts when the tree's lack it; the
/// file anchor, which that symbol verifies; a symbol the renamed file
/// lacks is searched for as a moved one; one the edit changed follows no
/// inexact rename.
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
        before: "R".to_owned(),
    };
    let renames = Renames {
        to: BTreeMap::from([
            ("src/old.rs".to_owned(), renamed("src/new.rs")),
            ("conf/old.toml".to_owned(), renamed("conf/new.toml")),
        ]),
        facts: facts(&[("conf/new.toml", "[profile]\npanic = 1\n")], &[]),
    };
    let history = prior(&[
        (
            "src/old.rs",
            "pub fn kept() {}\npub fn edited() { 1 }\npub fn left() {}\npub fn lost() {}\n",
        ),
        ("conf/old.toml", "[profile]\npanic = 1\n"),
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
            gone("src/old.rs", Some("src/new.rs")),
            gone("src/old.rs::edited", None),
            gone("src/old.rs::kept", Some("src/new.rs::kept")),
            gone("src/old.rs::left", Some("src/other.rs::left")),
            gone("src/old.rs::lost", None),
        ]
    );
}

/// git pairs two files by similarity alone, so a rename carries a file
/// anchor only when it is shown to: exact into a file that defines at least
/// one item, or followed by a symbol anchor whose code is unchanged. A
/// header-only or an empty file has no identity to carry, and a `main` the
/// rename edited is no evidence.
#[test]
fn a_file_anchor_follows_only_a_verified_rename() {
    let here = facts(
        &[
            MANIFEST,
            ("src/beta.rs", "// License.\n"),
            ("src/empty.rs", ""),
            ("src/bin/report.rs", "fn main() { report() }\n"),
            ("src/same.rs", "pub fn same() {}\n"),
        ],
        &[],
    );
    let renamed = |to: &str, exact| Renamed {
        to: to.to_owned(),
        exact,
        before: "R".to_owned(),
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
        ]),
        facts: facts(&[], &[]),
    };
    let history = prior(&[("src/bin/import.rs", "fn main() { import() }\n")]);
    let anchors = [
        "src/alpha.rs",
        "src/void.rs",
        "src/bin/import.rs",
        "src/bin/import.rs::main",
        "src/copy.rs",
    ];
    let decisions = [("cfg", decision(&anchors, StatusName::Accepted))];

    let lines = repaired(&decisions, &here, renames, history);

    let gone = "nothing is at its path";
    assert_eq!(
        lines,
        [
            format!("anchor `src/alpha.rs` of `cfg` is stale: {gone}"),
            format!("anchor `src/bin/import.rs` of `cfg` is stale: {gone}"),
            format!("anchor `src/bin/import.rs::main` of `cfg` is stale: {gone}"),
            format!("anchor `src/copy.rs` of `cfg` is stale: {gone}; repair: `src/same.rs`"),
            format!("anchor `src/void.rs` of `cfg` is stale: {gone}"),
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
            path: PathBuf::from("/w/branch"),
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
    let unresolved = here.unresolved(&named);
    let history = prior(&[("src/lib.rs", "pub fn gone() {}\npub fn branch() {}\n")]);
    let asked = Asked::of(&unresolved, &elsewhere).with_prior(history);

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
                | Kind::Mixed(_)
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
