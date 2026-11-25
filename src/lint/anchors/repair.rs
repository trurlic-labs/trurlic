//! The repair of a stale anchor: the anchor it would hold had it followed
//! its code, when the calling tree and git's history show exactly one. A
//! wrong repair hides the staleness it mends, and `repair_anchors` writes
//! every repair `check` shows unattended, so each rule below gives up
//! rather than guess.
//!
//! Only a stale anchor is repaired: a planned one names what is not
//! written yet, and a pending one's code is on a branch. An anchor whose
//! file is gone follows the rename git reports for that file
//! ([`Renames`]) when the rename is shown to carry its code: a symbol
//! anchor when its symbol resolves at the new path to definitions of the
//! kinds it named at the old path before the rename, and the rename is
//! exact (git's similarity 100) or the symbol's span hash there equals its
//! hash before; a file anchor when the rename is exact into a file that
//! defines at least one item, or a symbol anchor of any decision on the
//! old path followed it so, and, for an inexact rename, none that resolved
//! there before failed to. Two unrelated files git pairs by a shared
//! header or boilerplate are no rename of each other.
//!
//! Else, and for an anchor whose file defines nothing its symbol names, a
//! symbol on a Rust file is searched for where its code may have moved
//! ([`CodeFacts::namesakes`]): never when the anchor's own file still
//! defines its last segment, or spells it among the first tokens of a
//! macro invocation, since the code is then there under another spelling
//! or in a macro the index cannot expand; and only as the whole path of an
//! item written in another file of the same target by place, never a
//! re-export or a type a macro invocation defines. A file of another
//! language is not searched, since no crate or target bounds where its
//! code may go. The one such file is a [`Search`] candidate, and the
//! repair only when git's history shows the code moving there ([`Prior`]).
//! At the revision where the anchor's file last held its code
//! ([`Judged`]), that file defined the symbol and the candidate did not,
//! with the kinds and roles the candidate's definitions have now: a
//! function never lands on a `mod` declaration, nor code on test code, a
//! definition's role being its own under `#[cfg(test)]`, else its file's
//! in the tree. And the candidate gained the symbol where the code left
//! the anchor's file ([`Gain`]): in that same commit, or later from a file
//! that gained it in that commit and lost it then. A namesake that stood
//! beside the code, or one written after the code was gone, is then no
//! move; nor is the file a rename took the anchor's file to, which the
//! rename did not carry the symbol into unchanged. With no git, or any git
//! failure, no candidate is repaired.

use std::collections::{BTreeMap, BTreeSet};

use crate::code::{CodeFacts, DefinitionKind, Entry, FileKind, Namesakes, Resolution, Role};
use crate::store::Anchor;

use super::{Asked, Here};

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

    /// The kinds of the definitions whose whole path `anchor`'s symbol is
    /// in the tree, read as [`Renames::resolve`] reads it.
    fn kinds(&self, here: &CodeFacts, anchor: &Anchor) -> BTreeSet<DefinitionKind> {
        let now = here.whole(anchor);
        let now = match now.is_empty() {
            true => self.facts.whole(anchor),
            false => now,
        };
        now.into_iter().map(|(kind, _)| kind).collect()
    }
}

/// Where an anchored file last held what the tree no longer does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Judged {
    /// In `HEAD`: the work tree changed the file since.
    Uncommitted,
    /// In the parent of this commit, the last that changed the file.
    Before(String),
}

impl Judged {
    /// The revision the file is judged at, as git names it.
    pub(crate) fn revision(&self) -> String {
        match self {
            Self::Uncommitted => "HEAD".to_owned(),
            Self::Before(commit) => format!("{commit}^"),
        }
    }
}

/// What git's history holds of the files a repair is judged by, each file
/// read once at each revision: the old paths of renames before them; for a
/// search, where its anchor's file last held its code, the commits that
/// changed how often its candidate spells the symbol's name since, and the
/// paths some commits changed.
#[derive(Debug, Default)]
pub(crate) struct Prior {
    /// Where each anchored file is judged, by its path.
    at: BTreeMap<String, Judged>,
    /// The commits after an anchored file's revision, newest first, that
    /// changed how often a candidate spells a name, by the anchor's file,
    /// the candidate and the name.
    spelled: BTreeMap<(String, String, String), Vec<String>>,
    /// The paths each commit changed.
    changed: BTreeMap<String, BTreeSet<String>>,
    /// The facts of each file read, by revision and path.
    files: BTreeMap<(String, String), CodeFacts>,
}

impl Prior {
    /// The anchored file at `path` is judged where `judged` says.
    pub(crate) fn judge_at(&mut self, path: String, judged: Judged) {
        self.at.insert(path, judged);
    }

    /// Where the anchored file at `path` is judged.
    pub(crate) fn judged(&self, path: &str) -> Option<&Judged> {
        self.at.get(path)
    }

    /// `commits`, newest first, changed how often `target`, a search's
    /// candidate, spells `name` since the anchored `file` was judged.
    pub(crate) fn spelled(&mut self, file: &str, target: &str, name: &str, commits: Vec<String>) {
        let key = (file.to_owned(), target.to_owned(), name.to_owned());
        self.spelled.insert(key, commits);
    }

    /// The commits [`Prior::spelled`] holds for `search`.
    fn spelling(&self, search: &Search<'_>) -> Option<&Vec<String>> {
        self.spelled.get(&search.spelling())
    }

    /// `commit` changed `paths`.
    pub(crate) fn changed(&mut self, commit: String, paths: BTreeSet<String>) {
        self.changed.insert(commit, paths);
    }

    /// Each of `files` held its entry at its revision: a revision, a path
    /// and what was there, each file read alone.
    pub(crate) fn extend(&mut self, files: impl IntoIterator<Item = (String, String, Entry)>) {
        for (revision, path, entry) in files {
            // clone: the facts key their one entry by the path the map keys too.
            let facts = CodeFacts::new(BTreeMap::from([(path.clone(), entry)]), BTreeSet::new());
            self.files.insert((revision, path), facts);
        }
    }

    /// What `anchor` resolved to at `revision`; `FileMissing` for a file
    /// git held no blob of there, or that was not read.
    fn resolve(&self, revision: &str, anchor: &Anchor) -> Resolution {
        self.facts(revision, anchor)
            .map_or(Resolution::FileMissing, |facts| facts.resolve(anchor))
    }

    /// The kind and own role of each definition whose whole path `anchor`'s
    /// symbol was in its file at `revision`: none when the file did not
    /// define it, or was not read. A file read alone is no test file.
    fn whole(&self, revision: &str, anchor: &Anchor) -> BTreeSet<(DefinitionKind, Role)> {
        self.facts(revision, anchor)
            .map_or_else(BTreeSet::new, |facts| facts.whole(anchor))
    }

    /// The kinds of the definitions whose whole path `anchor`'s symbol was
    /// in its file at `revision`.
    fn kinds(&self, revision: &str, anchor: &Anchor) -> BTreeSet<DefinitionKind> {
        let whole = self.whole(revision, anchor).into_iter();
        whole.map(|(kind, _)| kind).collect()
    }

    /// Whether `anchor`'s file defined its symbol as a whole path at
    /// `revision`.
    fn defined(&self, revision: &str, anchor: &Anchor) -> bool {
        !self.whole(revision, anchor).is_empty()
    }

    /// Whether `anchor`'s file wrote an item whose whole path its symbol
    /// was at `revision`: no re-export, no module declaration.
    fn wrote(&self, revision: &str, anchor: &Anchor) -> bool {
        self.facts(revision, anchor)
            .is_some_and(|facts| facts.writes_whole(anchor))
    }

    /// The facts of `anchor`'s file at `revision`, when it was read.
    fn facts(&self, revision: &str, anchor: &Anchor) -> Option<&CodeFacts> {
        let key = (revision.to_owned(), anchor.file().to_owned());
        self.files.get(&key)
    }
}

/// A stale anchor the symbol search found one file for, `target`: the
/// repair when git's history shows the move.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Search<'s> {
    pub(crate) anchor: &'s Anchor,
    pub(crate) target: String,
    /// The name of the symbol's last segment.
    pub(crate) name: String,
    /// The role of the anchor's file in the tree.
    file_role: Role,
}

impl Search<'_> {
    /// What the commits that changed its candidate's spelling of the name
    /// are kept by: the anchor's file, the candidate and the name.
    fn spelling(&self) -> (String, String, String) {
        // clone: the key owns what the search borrows or keeps.
        let (target, name) = (self.target.clone(), self.name.clone());
        (self.anchor.file().to_owned(), target, name)
    }

    /// The anchor on its target.
    fn there(&self) -> Option<Anchor> {
        let symbol = self.anchor.symbol().map(str::to_owned);
        // clone: the repair owns its path and symbol.
        Anchor::new(self.target.clone(), symbol).ok()
    }
}

/// Where a search's candidate gained the symbol, as git's history shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Gain {
    /// In the work tree, which changed the anchor's file since `HEAD`.
    WorkTree,
    /// In the commit the anchor's file last changed in.
    AtLoss,
    /// In this later commit.
    Later(String),
}

/// A search and where its candidate gained the symbol.
#[derive(Debug)]
pub(crate) struct Gained<'s> {
    search: Search<'s>,
    gain: Gain,
}

/// The repair of each of `asked`'s stale anchors by the rename git reports
/// for its gone file, when the rename is shown to carry its code.
pub(crate) fn followed<'s>(
    asked: &Asked<'s>,
    here: &Here<'s, '_>,
    renames: &Renames,
    prior: &Prior,
) -> BTreeMap<&'s Anchor, Anchor> {
    let gone = |anchor: &Anchor| here.resolutions.get(anchor) == Some(&Resolution::FileMissing);
    let mut found = BTreeMap::new();
    let mut carried_from: BTreeSet<&str> = renames
        .to
        .iter()
        .filter(|(_, renamed)| {
            let to = renamed.to.as_str();
            renamed.exact && (here.facts.defines_anything(to) || renames.facts.defines_anything(to))
        })
        .map(|(from, _)| from.as_str())
        .collect();
    let mut refuted: BTreeSet<&str> = BTreeSet::new();
    let symbols = here.resolutions.keys().copied();
    for anchor in symbols.filter(|&a| a.symbol().is_some() && gone(a)) {
        if let Some(moved) = carried(anchor, here.facts, renames, prior) {
            carried_from.insert(anchor.file());
            if asked.stale.contains(anchor) {
                found.insert(anchor, moved);
            }
        } else if follow(anchor, here.facts, renames).is_none()
            && held_before(anchor, renames, prior)
        {
            refuted.insert(anchor.file());
        }
    }
    let files = asked.stale.iter().copied().filter(|anchor| {
        let file = anchor.file();
        anchor.symbol().is_none() && carried_from.contains(file) && !refuted.contains(file)
    });
    for anchor in files {
        if let Some(moved) = follow(anchor, here.facts, renames) {
            found.insert(anchor, moved);
        }
    }
    found
}

/// `anchor`, a symbol anchor, on the path its file was renamed to, when
/// its symbol resolves there and the rename carried its code: exact, or
/// the symbol's span hash equal to its hash at the old path before it;
/// and the definitions it names there are of the kinds it named there,
/// since an edit after an exact rename may have moved the code out and
/// left a declaration of its name.
fn carried(anchor: &Anchor, here: &CodeFacts, renames: &Renames, prior: &Prior) -> Option<Anchor> {
    let renamed = renames.to.get(anchor.file())?;
    let moved = follow(anchor, here, renames)?;
    let now = renames.resolve(here, &moved);
    let before = prior.resolve(&renamed.before, anchor);
    let unchanged = renamed.exact || now.same_code(&before);
    let kinds = prior.kinds(&renamed.before, anchor) == renames.kinds(here, &moved);
    (unchanged && kinds).then_some(moved)
}

/// Whether `anchor` resolved in its file before the inexact rename git
/// reports for it: one that does not resolve at the new path then shows
/// git's pairing is no move of the file.
fn held_before(anchor: &Anchor, renames: &Renames, prior: &Prior) -> bool {
    renames.to.get(anchor.file()).is_some_and(|renamed| {
        let before = prior.resolve(&renamed.before, anchor);
        !renamed.exact && matches!(before, Resolution::Resolved(_))
    })
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

/// The stale symbol anchors no rename repairs (`followed`) whose search
/// finds one file, each with it. The file a rename took the anchor's file
/// to is no candidate: the rename did not carry the symbol's code there
/// unchanged, so it was edited, not moved.
pub(crate) fn searched<'s>(
    asked: &Asked<'s>,
    here: &Here<'s, '_>,
    renames: &Renames,
    followed: &BTreeMap<&'s Anchor, Anchor>,
) -> Vec<Search<'s>> {
    let searched = asked.stale.iter().copied().filter(|&anchor| {
        let missing = matches!(
            here.resolutions.get(anchor),
            Some(Resolution::SymbolMissing | Resolution::FileMissing)
        );
        anchor.symbol().is_some() && missing && !followed.contains_key(anchor)
    });
    here.facts
        .namesakes(searched)
        .into_iter()
        .filter_map(|(anchor, namesakes)| {
            let Namesakes {
                at_home,
                elsewhere,
                name,
                file_role,
            } = namesakes;
            let [target] = <[String; 1]>::try_from(elsewhere).ok()?;
            let edited = renames
                .to
                .get(anchor.file())
                .is_some_and(|rename| rename.to == target);
            let search = Search {
                anchor,
                target,
                name,
                file_role,
            };
            (!at_home && !edited).then_some(search)
        })
        .collect()
}

/// What git's history must give of each of `searches` to say where its
/// candidate gained the symbol, a revision and a path each: the anchor's
/// file and the candidate where the anchor's file is judged, and the
/// candidate at each commit [`Prior::spelled`] lists and at its parent.
pub(crate) fn gain_reads(searches: &[Search<'_>], prior: &Prior) -> BTreeSet<(String, String)> {
    let mut reads = BTreeSet::new();
    for search in searches {
        let (file, target) = (search.anchor.file(), &search.target);
        let Some(judged) = prior.judged(file) else {
            continue;
        };
        reads.insert((judged.revision(), file.to_owned()));
        // clone: each read names its path.
        reads.insert((judged.revision(), target.clone()));
        for commit in prior.spelling(search).into_iter().flatten() {
            // clone: as above, at each commit and its parent.
            reads.insert((commit.clone(), target.clone()));
            // clone: as above.
            reads.insert((format!("{commit}^"), target.clone()));
        }
    }
    reads
}

/// Each of `searches` with where its candidate gained the symbol, when
/// git's history says so: a candidate that held it where the anchor's file
/// is judged, beside the code, gained nothing.
pub(crate) fn gained<'s>(searches: Vec<Search<'s>>, prior: &Prior) -> Vec<Gained<'s>> {
    searches
        .into_iter()
        .filter_map(|search| {
            let there = search.there()?;
            let gain = match prior.judged(search.anchor.file())? {
                Judged::Uncommitted => {
                    let held = prior.defined(&Judged::Uncommitted.revision(), &there);
                    (!held).then_some(Gain::WorkTree)?
                }
                Judged::Before(lost) => gain(&search, &there, lost, prior)?,
            };
            Some(Gained { search, gain })
        })
        .collect()
}

/// Where `search`'s candidate, `there` the anchor on it, gained the symbol
/// after the commit `lost`, the last that changed the anchor's file: the
/// newest commit that changed its spelling of the name whose parent lacked
/// the symbol, when the candidate defines it there and at every newer such
/// commit.
fn gain(search: &Search<'_>, there: &Anchor, lost: &str, prior: &Prior) -> Option<Gain> {
    for commit in prior.spelling(search)? {
        if !prior.defined(commit, there) {
            return None;
        }
        if !prior.defined(&format!("{commit}^"), there) {
            return Some(match commit == lost {
                true => Gain::AtLoss,
                // clone: the gain outlives the history it was read from.
                false => Gain::Later(commit.clone()),
            });
        }
    }
    None
}

/// The commits whose changed paths a later gain is judged by: the one the
/// anchor's file was lost in and the one the candidate gained it in.
pub(crate) fn later_commits<'g>(gained: &'g [Gained<'_>], prior: &'g Prior) -> BTreeSet<&'g str> {
    let mut commits = BTreeSet::new();
    for one in gained {
        let (Gain::Later(gain), Some(Judged::Before(lost))) =
            (&one.gain, prior.judged(one.search.anchor.file()))
        else {
            continue;
        };
        commits.insert(gain.as_str());
        commits.insert(lost.as_str());
    }
    commits
}

/// What git's history must give of each later gain of `gained` to say
/// whether a file carried the code between: each carrier where the
/// anchor's file is judged, in the commit it was lost in, and before the
/// gain.
pub(crate) fn carrier_reads(gained: &[Gained<'_>], prior: &Prior) -> BTreeSet<(String, String)> {
    let mut reads = BTreeSet::new();
    for one in gained {
        let (Gain::Later(gain), Some(Judged::Before(lost))) =
            (&one.gain, prior.judged(one.search.anchor.file()))
        else {
            continue;
        };
        for file in carriers(lost, gain, &one.search.target, prior) {
            reads.insert((format!("{lost}^"), file.to_owned()));
            // clone: each read names its revision.
            reads.insert((lost.clone(), file.to_owned()));
            reads.insert((format!("{gain}^"), file.to_owned()));
        }
    }
    reads
}

/// The Rust files other than `target` that both `lost`, the commit the
/// anchor's file lost the code in, and `gain` changed: where the code may
/// have been between.
fn carriers<'p>(
    lost: &str,
    gain: &str,
    target: &'p str,
    prior: &'p Prior,
) -> impl Iterator<Item = &'p str> {
    let gained = prior.changed.get(gain);
    let both = prior
        .changed
        .get(lost)
        .into_iter()
        .flatten()
        .filter(move |path| gained.is_some_and(|paths| paths.contains(*path)));
    both.map(String::as_str)
        .filter(move |&path| path != target && FileKind::of(path) == Some(FileKind::Rust))
}

/// The repair of each of `gained` that git's history shows moved, by what
/// the tree holds now, `here`.
pub(crate) fn moved<'s>(
    gained: &[Gained<'s>],
    here: &CodeFacts,
    prior: &Prior,
) -> BTreeMap<&'s Anchor, Anchor> {
    gained
        .iter()
        .filter_map(|one| Some((one.search.anchor, shown_moved(one, here, prior)?)))
        .collect()
}

/// `gained`'s anchor on its candidate, when git's history shows the move:
/// where the anchor's file is judged it defined the symbol as definitions
/// of the kinds and roles the candidate's have now, which are some; and
/// the candidate gained it in the commit the anchor's file lost it, or
/// later from a carrier.
fn shown_moved(gained: &Gained<'_>, here: &CodeFacts, prior: &Prior) -> Option<Anchor> {
    let Gained { search, gain } = gained;
    let anchor = search.anchor;
    let judged = prior.judged(anchor.file())?;
    let there = search.there()?;
    let before: BTreeSet<(DefinitionKind, Role)> = prior
        .whole(&judged.revision(), anchor)
        .into_iter()
        .map(|(kind, role)| (kind, role.or(search.file_role)))
        .collect();
    if before != here.whole(&there) {
        return None;
    }
    let moved = match (gain, judged) {
        (Gain::WorkTree | Gain::AtLoss, Judged::Uncommitted | Judged::Before(_)) => true,
        (Gain::Later(gain), Judged::Before(lost)) => carried_between(search, lost, gain, prior),
        (Gain::Later(_), Judged::Uncommitted) => false,
    };
    moved.then_some(there)
}

/// Whether a file both `lost` and `gain` changed carried the code between:
/// it wrote no item of the symbol where the anchor's file is judged, wrote
/// one in `lost`, and still before `gain`. A re-export carries no code.
fn carried_between(search: &Search<'_>, lost: &str, gain: &str, prior: &Prior) -> bool {
    carriers(lost, gain, &search.target, prior).any(|file| {
        let symbol = search.anchor.symbol().map(str::to_owned);
        let Ok(carrier) = Anchor::new(file.to_owned(), symbol) else {
            return false;
        };
        !prior.wrote(&format!("{lost}^"), &carrier)
            && prior.wrote(lost, &carrier)
            && prior.wrote(&format!("{gain}^"), &carrier)
    })
}
