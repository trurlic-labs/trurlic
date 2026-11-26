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
//! the commit whose parent's file wrote the item, or the work tree. Each
//! such file of the anchor's language that writes the symbol's whole path
//! now is a [`Search`], and the move is the one of them that, across the
//! loss, went from writing no item of the symbol to definitions of the
//! span hash the anchor's file lost ([`moved_to`]): none, or two, and no
//! repair. That evidence is the code itself in the commit it moved in, so
//! a namesake elsewhere, in another crate, target or test module, which
//! that change did not write, is never one, and no bound by crate or
//! target is needed. The one that moved is the repair when it held
//! definitions of the kinds it gained after every change since and holds
//! them now: one a later change deleted, or replaced by another kind of
//! item, is no move, while an edit of the moved item keeps its repair.
//! With no git, or any git failure, no anchor is repaired.

use std::collections::{BTreeMap, BTreeSet};

use crate::code::{Code, CodeFacts, DefinitionKind, Entry, Resolution};
use crate::store::Anchor;

use super::{Asked, Here};

/// A stale anchor's repair: the anchor it is replaced by, and the code that
/// anchor names in the tree it was judged in, which the write holds it to
/// again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Repair {
    pub(crate) to: Anchor,
    pub(crate) code: Code,
}

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
        .stale
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
    let missing = asked.stale.iter().copied().filter(|&anchor| {
        let missing = matches!(
            here.resolutions.get(anchor),
            Some(Resolution::SymbolMissing | Resolution::FileMissing)
        );
        anchor.symbol().is_some() && missing
    });
    here.facts.writers(missing)
}

/// Of `versions`, the anchor's file before each of its newest
/// changes, newest first, as names of `prior`: the place of the first that
/// wrote an item of the anchor's symbol, which the change after it lost.
pub(crate) fn lost<'v>(
    anchor: &Anchor,
    versions: impl IntoIterator<Item = &'v str>,
    prior: &Prior,
) -> Option<usize> {
    versions
        .into_iter()
        .position(|version| prior.wrote(version, anchor))
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
