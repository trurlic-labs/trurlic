//! The repair of a stale anchor: the anchor it would hold had it followed
//! its code, when the calling tree and git's history show exactly one. A
//! wrong repair hides the staleness it mends, so each rule below gives up
//! rather than guess.
//!
//! Only a stale anchor is repaired: a planned one names what is not
//! written yet, and a pending one's code is on a branch. An anchor whose
//! file is gone follows the rename git reports for that file
//! ([`Renames`]) when the rename is shown to carry its code: a symbol
//! anchor when its symbol resolves at the new path and the rename is exact
//! (git's similarity 100) or the symbol's span hash there equals its hash
//! at the old path before the rename; a file anchor when the rename is
//! exact and the new file defines at least one item, or a symbol anchor of
//! any decision on the old path followed it so. Two unrelated files git
//! pairs by a shared header or boilerplate are no rename of each other.
//!
//! Else, and for an anchor whose file defines nothing its symbol names, a
//! symbol on a Rust file is searched for where its code may have moved
//! ([`CodeFacts::namesakes`]): never when the anchor's own file still
//! defines its last segment, or spells it among the first tokens of a
//! macro invocation, since the code is then there under another spelling
//! or in a macro the index cannot expand; and only as the whole path of an
//! item written in a file of the same crate target and test role, never a
//! re-export or a type a macro invocation defines. A file of another
//! language is not searched, since no crate or target bounds where its
//! code may go. The one such file is a [`Search`] candidate, and the
//! repair only when git's history shows the move ([`Prior`]): at the
//! revision where the anchor's own file last held its code, that file
//! defined the symbol and the candidate did not. A namesake that stood
//! beside the anchor's code is no move, and nor is the file a rename took
//! the anchor's file to, which the rename did not carry the symbol into
//! unchanged. With no git, or any git failure, no candidate is repaired.

use std::collections::{BTreeMap, BTreeSet};

use crate::code::{CodeFacts, Entry, Namesakes, Resolution};
use crate::store::Anchor;

/// What git says of the anchored files that are gone: where each went,
/// which the tree holds, and the facts of those paths.
#[derive(Debug)]
pub(crate) struct Renames {
    pub(crate) to: BTreeMap<String, Renamed>,
    pub(crate) facts: CodeFacts,
}

/// Where a gone path went, followed through each rename git reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Renamed {
    pub(crate) to: String,
    /// Whether every rename on the way kept the content whole: git's
    /// similarity 100.
    pub(crate) exact: bool,
    /// The revision before the first rename: `HEAD` for one not committed,
    /// else the parent of the commit that made it.
    pub(crate) before: String,
}

impl Renames {
    /// No file renamed.
    pub(crate) fn none() -> Self {
        Self {
            to: BTreeMap::new(),
            facts: CodeFacts::new(BTreeMap::new(), BTreeSet::new()),
        }
    }

    /// What `anchor` resolves to in the tree: from the calling tree's
    /// facts, or, for a path only a rename brought in, from those of the
    /// renamed paths.
    fn resolve(&self, here: &CodeFacts, anchor: &Anchor) -> Resolution {
        match here.resolve(anchor) {
            Resolution::FileMissing => self.facts.resolve(anchor),
            found @ (Resolution::Resolved(_)
            | Resolution::SymbolMissing
            | Resolution::Ambiguous(_)
            | Resolution::SymbolOnDirectory
            | Resolution::NoResolver
            | Resolution::Unindexable(_)
            | Resolution::Malformed(_)) => found,
        }
    }

    /// Whether the file at `path` in the tree defines at least one item.
    fn defines_anything(&self, here: &CodeFacts, path: &str) -> bool {
        here.defines_anything(path) || self.facts.defines_anything(path)
    }
}

/// What git's history held of the files a repair is judged by, each at
/// one revision, read once: the files of stale anchors and their search
/// candidates at the revision where the anchor's own file last held its
/// code, and the old paths of renames before them.
#[derive(Debug, Default)]
pub(crate) struct Prior {
    /// The revision each anchored file is judged at, by its path.
    at: BTreeMap<String, String>,
    /// The facts of each file read, by revision and path.
    files: BTreeMap<(String, String), CodeFacts>,
}

impl Prior {
    /// `revision` is where the anchored file at `path` last held its code.
    pub(crate) fn judge_at(&mut self, path: String, revision: String) {
        self.at.insert(path, revision);
    }

    /// The revision the anchored file at `path` is judged at.
    pub(crate) fn revision(&self, path: &str) -> Option<&str> {
        self.at.get(path).map(String::as_str)
    }

    /// The file at `path` held `entry` at `revision`.
    pub(crate) fn insert(&mut self, revision: String, path: String, entry: Entry) {
        // clone: the facts key their one entry by the path the map keys too.
        let facts = CodeFacts::new(BTreeMap::from([(path.clone(), entry)]), BTreeSet::new());
        self.files.insert((revision, path), facts);
    }

    pub(crate) fn extend(&mut self, other: Self) {
        self.at.extend(other.at);
        self.files.extend(other.files);
    }

    /// What `anchor` resolved to at `revision`; `FileMissing` for a file
    /// git held no blob of there, or that was not read.
    fn resolve(&self, revision: &str, anchor: &Anchor) -> Resolution {
        let key = (revision.to_owned(), anchor.file().to_owned());
        self.files
            .get(&key)
            .map_or(Resolution::FileMissing, |facts| facts.resolve(anchor))
    }

    /// Whether the file of `anchor` defined its symbol as a whole path at
    /// `revision`.
    fn defined(&self, revision: &str, anchor: &Anchor) -> bool {
        let key = (revision.to_owned(), anchor.file().to_owned());
        self.files
            .get(&key)
            .is_some_and(|facts| facts.defines_whole(anchor))
    }
}

/// A stale anchor the symbol search found one file for, `target`: the
/// repair when git's history shows the move.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Search<'s> {
    pub(crate) anchor: &'s Anchor,
    pub(crate) target: String,
}

/// The repair of each of `stale`, the anchors judged stale, by what every
/// anchor of the decisions resolves to here (`resolutions`), the renames
/// git reports and what its history held (`prior`), when they show one.
pub(super) fn repairs<'s>(
    stale: &BTreeSet<&'s Anchor>,
    resolutions: &BTreeMap<&'s Anchor, Resolution>,
    here: &CodeFacts,
    renames: &Renames,
    prior: &Prior,
) -> BTreeMap<&'s Anchor, Anchor> {
    let mut found = followed(stale, resolutions, here, renames, prior);
    for search in candidates(stale, resolutions, here, renames, &found) {
        if let Some(moved) = shown_moved(&search, prior) {
            found.insert(search.anchor, moved);
        }
    }
    found
}

/// The repair of each of `stale` by the rename git reports for its gone
/// file, when the rename is shown to carry its code.
pub(super) fn followed<'s>(
    stale: &BTreeSet<&'s Anchor>,
    resolutions: &BTreeMap<&'s Anchor, Resolution>,
    here: &CodeFacts,
    renames: &Renames,
    prior: &Prior,
) -> BTreeMap<&'s Anchor, Anchor> {
    let gone = |anchor: &Anchor| resolutions.get(anchor) == Some(&Resolution::FileMissing);
    let mut found = BTreeMap::new();
    let mut verified: BTreeSet<&str> = renames
        .to
        .iter()
        .filter(|(_, renamed)| renamed.exact && renames.defines_anything(here, &renamed.to))
        .map(|(from, _)| from.as_str())
        .collect();
    let symbols = resolutions
        .keys()
        .copied()
        .filter(|&a| a.symbol().is_some() && gone(a));
    for anchor in symbols {
        let Some(moved) = carried(anchor, here, renames, prior) else {
            continue;
        };
        verified.insert(anchor.file());
        if stale.contains(anchor) {
            found.insert(anchor, moved);
        }
    }
    let files = stale.iter().copied().filter(|a| a.symbol().is_none());
    for anchor in files.filter(|a| verified.contains(a.file())) {
        if let Some(moved) = follow(anchor, here, renames) {
            found.insert(anchor, moved);
        }
    }
    found
}

/// `anchor`, a symbol anchor, on the path its file was renamed to, when
/// its symbol resolves there and the rename carried its code: exact, or
/// the symbol's span hash equal to its hash at the old path before it.
fn carried(anchor: &Anchor, here: &CodeFacts, renames: &Renames, prior: &Prior) -> Option<Anchor> {
    let renamed = renames.to.get(anchor.file())?;
    let moved = follow(anchor, here, renames)?;
    let now = renames.resolve(here, &moved);
    let before = prior.resolve(&renamed.before, anchor);
    (renamed.exact || now.same_code(&before)).then_some(moved)
}

/// `anchor` on the path git says its file was renamed to, when what it
/// names resolves there.
fn follow(anchor: &Anchor, here: &CodeFacts, renames: &Renames) -> Option<Anchor> {
    let renamed = renames.to.get(anchor.file())?;
    // clone: the repair owns its path and symbol.
    let moved = Anchor::new(renamed.to.clone(), anchor.symbol().map(str::to_owned)).ok()?;
    let resolved = matches!(renames.resolve(here, &moved), Resolution::Resolved(_));
    resolved.then_some(moved)
}

/// The stale symbol anchors no rename repairs (`renamed`) whose search
/// finds one file, each with it. The file a rename took the anchor's file
/// to is no candidate: the rename did not carry the symbol's code there
/// unchanged, so it was edited, not moved.
pub(super) fn candidates<'s>(
    stale: &BTreeSet<&'s Anchor>,
    resolutions: &BTreeMap<&'s Anchor, Resolution>,
    here: &CodeFacts,
    renames: &Renames,
    renamed: &BTreeMap<&'s Anchor, Anchor>,
) -> Vec<Search<'s>> {
    let searched = stale.iter().copied().filter(|&anchor| {
        let missing = matches!(
            resolutions.get(anchor),
            Some(Resolution::SymbolMissing | Resolution::FileMissing)
        );
        anchor.symbol().is_some() && missing && !renamed.contains_key(anchor)
    });
    here.namesakes(searched)
        .into_iter()
        .filter_map(|(anchor, namesakes)| {
            let target = one_file(namesakes)?;
            let rename = renames.to.get(anchor.file());
            let edited = rename.is_some_and(|rename| rename.to == target);
            (!edited).then_some(Search { anchor, target })
        })
        .collect()
}

/// The one file `namesakes` names, when the anchor's own file holds no
/// definition of its last segment.
fn one_file(namesakes: Namesakes) -> Option<String> {
    let Namesakes { at_home, elsewhere } = namesakes;
    let [file] = <[String; 1]>::try_from(elsewhere).ok()?;
    (!at_home).then_some(file)
}

/// `search`'s anchor on its target, when git's history shows the move:
/// at the revision the anchor's file is judged at, that file defined the
/// symbol and the target did not.
fn shown_moved(search: &Search<'_>, prior: &Prior) -> Option<Anchor> {
    let anchor = search.anchor;
    let revision = prior.revision(anchor.file())?;
    // clone: the repair owns its path and symbol.
    let there = Anchor::new(search.target.clone(), anchor.symbol().map(str::to_owned)).ok()?;
    let moved = prior.defined(revision, anchor) && !prior.defined(revision, &there);
    moved.then_some(there)
}
