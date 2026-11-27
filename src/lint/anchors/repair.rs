//! The repair of a stale anchor: the anchor it would hold had it followed
//! its code, when the calling tree and git's history show the code itself
//! there. A wrong repair hides the staleness it mends, and
//! `repair_anchors` writes every repair `check` shows unattended, so a
//! repair is given only on evidence of the code itself, never of a name;
//! an anchor without such evidence stays stale, as it is.
//!
//! Only a stale anchor is repaired: a planned one names what is not
//! written yet, and a pending one's code is on a branch. A file anchor
//! whose file is gone follows that file where git's history shows its
//! blob moved, unchanged, as the shell follows it ([`Renames`]), into a
//! file that defines an item: a file of comments or nothing alone is no
//! code to follow, and a file git pairs with it by similarity alone, as a
//! shared header pairs two, is no move of it.
//!
//! A symbol anchor whose file is gone or defines nothing its symbol names
//! ([`missing`]) is looked for only in the files the change that lost its
//! code changed ([`lost`]), each followed by its blob to where it is now:
//! the newest change whose file before wrote the item and after it writes
//! none, or was deleted, in a commit or the work tree; a change whose file
//! still writes it lost nothing, whatever a merge did later. Each such
//! file of the anchor's file kind (`.ts`, `.d.ts` and `.tsx`, with
//! JavaScript, are kinds apart) that writes the symbol's whole path now is
//! a [`Search`], and the move is the one of them that, across the loss,
//! went from writing no item of the symbol to definitions of the span hash
//! the anchor's file lost ([`moved_to`]): none, or two, and no repair. A
//! file git does not track is no candidate and no second one, since the
//! history names no change of it. That evidence is the code itself in the
//! commit it moved in, so a namesake elsewhere, in another crate, target
//! or test module, which that change did not write, is never one, and no
//! bound by crate or target is needed. The one that moved is the repair
//! when it held definitions of the kinds it gained after every change
//! since and holds them now: one a later change deleted, or replaced by
//! another kind of item, is no move, while an edit of the moved item keeps
//! its repair.
//! With no git, or any git failure, no anchor is repaired.

use std::collections::{BTreeMap, BTreeSet};

use crate::code::{CodeFacts, DefinitionKind, Entry, Resolution};
use crate::store::Anchor;

use super::finding::Repair;
use super::trees::{Asked, Here};

/// Where git's history says each gone file of a stale anchor is now, and
/// the facts of those paths.
#[derive(Debug)]
pub(crate) struct Renames {
    pub(crate) to: BTreeMap<String, String>,
    pub(crate) facts: CodeFacts,
}

/// Files of git's history, each read alone, by the name git read it by and
/// its path.
#[derive(Debug, Default)]
pub(crate) struct Prior {
    files: BTreeMap<(String, String), CodeFacts>,
}

impl Prior {
    /// Each of `files` held its entry: a name git read it by, a path and
    /// what was there.
    pub(crate) fn extend(&mut self, files: impl IntoIterator<Item = (String, String, Entry)>) {
        for (name, path, entry) in files {
            // clone: the facts key their one entry by the path the map keys too.
            let facts = CodeFacts::new(BTreeMap::from([(path.clone(), entry)]), BTreeSet::new());
            self.files.insert((name, path), facts);
        }
    }

    /// Only the files of `keep`, each a name git read it by and its path.
    pub(crate) fn retain(&mut self, keep: &BTreeSet<(String, String)>) {
        self.files.retain(|file, _| keep.contains(file));
    }

    /// The facts of `anchor`'s file as `name` held it, when it was read.
    fn facts(&self, name: &str, anchor: &Anchor) -> Option<&CodeFacts> {
        let key = (name.to_owned(), anchor.file().to_owned());
        self.files.get(&key)
    }

    /// What `anchor` resolved to in `name`; `FileMissing` for a file not
    /// read.
    fn resolve(&self, name: &str, anchor: &Anchor) -> Resolution {
        self.facts(name, anchor)
            .map_or(Resolution::FileMissing, |facts| facts.resolve(anchor))
    }

    /// Whether `name` wrote an item whose whole path `anchor`'s symbol is:
    /// no re-export, no module declaration.
    fn wrote(&self, name: &str, anchor: &Anchor) -> bool {
        self.facts(name, anchor)
            .is_some_and(|facts| facts.writes_whole(anchor))
    }

    /// The kinds of the definitions of `name` whose whole path `anchor`'s
    /// symbol is: none for a file not read.
    fn kinds(&self, name: &str, anchor: &Anchor) -> BTreeSet<DefinitionKind> {
        self.facts(name, anchor)
            .map_or_else(BTreeSet::new, |facts| facts.kinds(anchor))
    }
}

/// The repair of each of `asked`'s stale file anchors whose gone file git's
/// history shows at another path now, `renames`, when that file defines an
/// item.
pub(crate) fn followed<'s>(
    asked: &Asked<'s>,
    here: &Here<'s, '_>,
    renames: &Renames,
) -> BTreeMap<&'s Anchor, Repair> {
    asked
        .stale()
        .iter()
        .copied()
        .filter(|&anchor| {
            let gone = here.resolutions.get(anchor) == Some(&Resolution::FileMissing);
            anchor.symbol().is_none() && gone
        })
        .filter_map(|anchor| {
            let to = renames.to.get(anchor.file())?;
            if !renames.facts.defines_anything(to) {
                return None;
            }
            // clone: the repair owns its path.
            let to = Anchor::new(to.clone(), None).ok()?;
            let code = renames.facts.code(&to)?;
            Some((anchor, Repair { to, code }))
        })
        .collect()
}

/// A stale symbol anchor and its symbol on a file the change that lost
/// its code changed: the repair when that file shows the move.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Search<'s> {
    pub(crate) anchor: &'s Anchor,
    pub(crate) there: Anchor,
}

/// The stale symbol anchors whose file is gone or defines nothing their
/// symbol names, each with the other files of its kind that write its
/// symbol here: those whose code is looked for where it moved, and the
/// only files it can be in now.
pub(crate) fn missing<'s, 'f>(
    asked: &Asked<'s>,
    here: &Here<'s, 'f>,
) -> BTreeMap<&'s Anchor, BTreeSet<&'f str>> {
    let missing = asked.stale().iter().copied().filter(|&anchor| {
        let missing = matches!(
            here.resolutions.get(anchor),
            Some(Resolution::SymbolMissing | Resolution::FileMissing)
        );
        anchor.symbol().is_some() && missing
    });
    here.facts.writers(missing)
}

/// Of `versions`, the anchor's file before and after each of its newest
/// changes, newest first, as names of `prior` (after, `None` where the
/// change left no file that writes the symbol, a deletion or the work
/// tree's): the place of the first change that lost an item of the
/// anchor's symbol, its file writing one before and none after. A change
/// whose file still writes it lost nothing, whatever a merge did later.
pub(crate) fn lost<'v>(
    anchor: &Anchor,
    versions: impl IntoIterator<Item = (&'v str, Option<&'v str>)>,
    prior: &Prior,
) -> Option<usize> {
    let wrote = |version: &str| prior.wrote(version, anchor);
    versions
        .into_iter()
        .position(|(before, after)| wrote(before) && !after.is_some_and(wrote))
}

/// What git's history shows of a search's candidate, as names of
/// [`Prior`], around the change where the anchor's file lost the code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Shown {
    /// The anchor's file before the loss.
    pub(crate) lost: String,
    /// The candidate before the loss; `None` where no file was there.
    pub(crate) before: Option<String>,
    /// The candidate after the loss; `None` for a loss in the work
    /// tree, whose file is the tree's.
    pub(crate) after: Option<String>,
    /// The candidate after each commit that changed it since the loss.
    pub(crate) since: Vec<String>,
}

/// The repair of the one of `searches`, the files the loss changed, that
/// git's history shows the code moved into, judged by `shown` and what the
/// tree holds now, `here`: across the loss it went from writing no item of
/// the symbol to definitions with the span hash the anchor's had before
/// it; and it held definitions of the kinds it gained after every change
/// since and holds them now. None when no file, or more than one, shows
/// the move.
pub(crate) fn moved_to(
    searches: &[(Search<'_>, Shown)],
    here: &CodeFacts,
    prior: &Prior,
) -> Option<Repair> {
    let mut gained = searches.iter().filter_map(|(search, shown)| {
        let kinds = gained(search, shown, here, prior)?;
        Some((search, shown, kinds))
    });
    let (Some((search, shown, kinds)), None) = (gained.next(), gained.next()) else {
        return None;
    };
    let there = &search.there;
    let kept = shown.since.iter().all(|at| prior.kinds(at, there) == kinds);
    if !kept || here.kinds(there) != kinds {
        return None;
    }
    let code = here.code(there)?;
    Some(Repair {
        // clone: the repair owns its anchor.
        to: there.clone(),
        code,
    })
}

/// The kinds of the definitions `search`'s candidate gained across the
/// loss `shown`, when it wrote no item of the symbol before and holds the
/// span hash the anchor's file lost after it.
fn gained(
    search: &Search<'_>,
    shown: &Shown,
    here: &CodeFacts,
    prior: &Prior,
) -> Option<BTreeSet<DefinitionKind>> {
    let Search { anchor, there } = search;
    let held_before = shown
        .before
        .as_ref()
        .is_some_and(|before| prior.wrote(before, there));
    let (after, kinds) = match &shown.after {
        Some(after) => (prior.resolve(after, there), prior.kinds(after, there)),
        None => (here.resolve(there), here.kinds(there)),
    };
    let same = prior.resolve(&shown.lost, anchor).same_code(&after);
    (!held_before && same && !kinds.is_empty()).then_some(kinds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::testing::facts;
    use crate::code::{Entry, FileFault, FileKind};
    use crate::lint::anchors::testing::{anchor, decision};
    use crate::lint::anchors::{Asked, Elsewhere, Here, Worktree, findings, repair};
    use crate::store::{Decision, Status, StatusName};

    /// The manifest of a package at the root of the tree.
    const MANIFEST: (&str, &str) = ("Cargo.toml", "[package]\nname = \"p\"\n");

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
            let index =
                crate::code::testing::index(kind, text.as_bytes()).map_err(FileFault::Index);
            (
                name.to_owned(),
                path.to_owned(),
                Entry::File {
                    kind,
                    lines: Some(crate::code::line_count(text.as_bytes())),
                    index,
                },
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
    /// Two files of the loss that both gained the code are no move.
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

            // clone: the same search is judged alone and beside a twin.
            let alone = repair::moved_to(&[(search.clone(), shown.clone())], &now, &held);
            let twin = (search.clone(), shown.clone());
            let twice = repair::moved_to(&[(search.clone(), shown), twin], &now, &held);

            let to = self::anchor("src/b.rs::foo");
            let expected = moved.then(|| Repair {
                code: now.code(&to).unwrap(),
                to,
            });
            assert_eq!(alone, expected, "case {at}");
            assert_eq!(twice, None, "case {at}");
        }
    }

    /// Where a file lost the code is the newest of its changes whose file
    /// before wrote an item of the symbol and after it writes none, or was
    /// deleted: a change that kept the item, as a side branch's edit a merge
    /// later dropped, lost nothing.
    #[test]
    fn the_loss_is_the_newest_change_whose_file_before_wrote_the_item_and_after_none() {
        let held = prior(&[
            ("y^", "src/a.rs", "pub fn a() {}\n"),
            ("y", "src/a.rs", "pub fn b() {}\n"),
            ("x^", "src/a.rs", FOO),
            ("x", "src/a.rs", &format!("{FOO}pub fn more() {{}}\n")),
            ("w^", "src/a.rs", FOO),
            ("w", "src/a.rs", "pub fn a() {}\n"),
        ]);
        let anchor = anchor("src/a.rs::foo");
        let versions = [("y^", Some("y")), ("x^", Some("x")), ("w^", Some("w"))];

        assert_eq!(repair::lost(&anchor, versions, &held), Some(2));
        assert_eq!(repair::lost(&anchor, [("x^", None)], &held), Some(0));
        assert_eq!(repair::lost(&anchor, [("y^", None)], &held), None);
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
    /// code is test code, repaired to the test module it moved into.
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

        let shown = shown(Some("none"), Some("gained"), &[]);
        let repair = repair::moved_to(&[(search, shown)], &here, &held);

        let to = "src/b.rs::tests::fixture".to_owned();
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
        let found = repair::missing(&asked, &here);
        let gone = anchor("src/lib.rs::gone");
        let to = anchor("src/moved.rs::gone");
        let code = here.facts.code(&to).unwrap();
        let asked = asked.with_repairs(BTreeMap::from([(&gone, Repair { to, code })]));

        let lines: Vec<String> = findings(&named, &here, &asked)
            .iter()
            .map(ToString::to_string)
            .collect();

        let searched: Vec<String> = found.keys().map(ToString::to_string).collect();
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
}
