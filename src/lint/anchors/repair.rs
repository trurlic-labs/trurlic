//! The repair of a stale anchor: the anchor it would hold had it followed
//! its code, when the calling tree and git's history show the code itself
//! there. A wrong repair hides the staleness it mends, and
//! `repair_anchors` writes every repair `check` shows unattended, so a
//! repair is given only where the anchor's definitions and the repair's
//! have the same span hash: the same tokens, comments and whitespace
//! aside, nothing else tolerated, not even a rename of the item.
//!
//! Only a stale anchor is repaired: a planned one names what is not
//! written yet, and a pending one's code is on a branch. An anchor whose
//! file is gone follows the rename git reports for that file
//! ([`Renames`]): a symbol anchor when its definitions before the rename
//! have the span hash they have in the file the rename made, and its symbol
//! resolves at the new path now to definitions of the kinds it named
//! there; a file anchor when the rename is exact into a file that defines
//! an item, or one of its symbol anchors followed so, and, for an inexact
//! rename, no symbol anchor that resolved in the old file is missing from
//! the file the rename made. Two unrelated files git pairs by a shared
//! header or boilerplate are no rename of each other.
//!
//! Else, and for an anchor whose file defines nothing its symbol names, a
//! symbol on a Rust file is searched for where its code may have moved
//! ([`CodeFacts::namesakes`]), in the file the rename made when its file
//! was renamed: never when that file still defines its last segment, or
//! spells the name in a macro invocation where an item stands
//! ([`Resolution::SymbolInInvocation`]), since the code is then there under
//! another spelling or in an invocation, which defines nothing; and only
//! as the whole path of an item written in another file of the same target
//! by place, never a re-export or a module declaration, and never an item
//! an invocation produces, which is no definition. A file of another
//! language is not searched, since no crate or target bounds where its
//! code may go. The one such file is a [`Search`] candidate. The repair is
//! given when the history shows the move in one step ([`Loss`]): the
//! commit where the searched file last lost an item of the symbol, or the
//! work tree, gave the candidate an item of the symbol it did not write
//! before, with the span hash the searched file's definitions had. A
//! namesake that stood beside the code, or one written later, is no move.
//! With no git, or any git failure, no candidate is repaired.

use std::collections::{BTreeMap, BTreeSet};

use crate::code::{CodeFacts, DefinitionKind, Entry, Namesakes, Resolution, Role};
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
    /// The commit of the last rename, whose blob at `to` is the file the
    /// rename made; `None` for a rename not committed, whose file is the
    /// work tree's.
    pub(crate) made: Option<String>,
}

impl Renames {
    /// No file renamed.
    pub(crate) fn none() -> Self {
        Self {
            to: BTreeMap::new(),
            facts: CodeFacts::new(BTreeMap::new(), BTreeSet::new()),
        }
    }

    /// The facts `anchor` is read by in the tree: the calling tree's, or,
    /// for a path only a rename brought in, those of the renamed paths.
    fn facts<'a>(&'a self, here: &'a CodeFacts, anchor: &Anchor) -> &'a CodeFacts {
        match here.resolve(anchor) {
            Resolution::FileMissing => &self.facts,
            Resolution::Resolved(_)
            | Resolution::SymbolMissing
            | Resolution::SymbolInInvocation
            | Resolution::Ambiguous(_)
            | Resolution::SymbolOnDirectory
            | Resolution::NoResolver
            | Resolution::Unindexable(_)
            | Resolution::Malformed(_) => here,
        }
    }

    /// What `anchor` resolves to in the tree.
    fn resolve(&self, here: &CodeFacts, anchor: &Anchor) -> Resolution {
        self.facts(here, anchor).resolve(anchor)
    }

    /// The kinds of the definitions whose whole path `anchor`'s symbol is
    /// in the tree.
    fn kinds(&self, here: &CodeFacts, anchor: &Anchor) -> BTreeSet<DefinitionKind> {
        let whole = self.facts(here, anchor).whole(anchor);
        whole.into_iter().map(|(kind, _)| kind).collect()
    }
}

/// What git's history holds of the files a repair is judged by, each file
/// read once at each revision; for a searched file, the commits that
/// changed it, newest first, as far as they are read, and whether the work
/// tree changed it since `HEAD`.
#[derive(Debug, Default)]
pub(crate) struct Prior {
    /// The facts of each file read, by revision and path.
    files: BTreeMap<(String, String), CodeFacts>,
    commits: BTreeMap<String, Vec<String>>,
    uncommitted: BTreeSet<String>,
    /// For each candidate, the commits that changed it, newest first, and
    /// whether each added or deleted it.
    targets: BTreeMap<String, Vec<(String, bool)>>,
}

impl Prior {
    /// Each of `files` held its entry at its revision: a revision, a path
    /// and what was there, each file read alone.
    pub(crate) fn extend(&mut self, files: impl IntoIterator<Item = (String, String, Entry)>) {
        for (revision, path, entry) in files {
            // clone: the facts key their one entry by the path the map keys too.
            let facts = CodeFacts::new(BTreeMap::from([(path.clone(), entry)]), BTreeSet::new());
            self.files.insert((revision, path), facts);
        }
    }

    /// `commits`, newest first, changed the searched file at `path`, and
    /// the work tree changed it since `HEAD` when `uncommitted` says so.
    pub(crate) fn changed(&mut self, path: String, commits: Vec<String>, uncommitted: bool) {
        if uncommitted {
            // clone: the two maps each key by the path.
            self.uncommitted.insert(path.clone());
        }
        self.commits.insert(path, commits);
    }

    /// `changes`, newest first, changed the candidate at `path`, each with
    /// whether it added or deleted the path.
    pub(crate) fn target_changed(&mut self, path: String, changes: Vec<(String, bool)>) {
        self.targets.insert(path, changes);
    }

    /// Whether the file at `path` has been the file `commit` changed ever
    /// since: no newer commit added or deleted it.
    fn kept_since(&self, path: &str, commit: &str) -> bool {
        let Some(changes) = self.targets.get(path) else {
            return false;
        };
        let Some(at) = changes.iter().position(|(c, _)| c == commit) else {
            return false;
        };
        changes.iter().take(at).all(|&(_, replaced)| !replaced)
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
    /// The anchor's symbol on the file whose history is searched: its own,
    /// or the one its rename made.
    pub(crate) at: Anchor,
    pub(crate) target: String,
}

impl Search<'_> {
    /// The anchor on its target.
    fn there(&self) -> Option<Anchor> {
        let symbol = self.anchor.symbol().map(str::to_owned);
        // clone: the repair owns its path and symbol.
        Anchor::new(self.target.clone(), symbol).ok()
    }
}

/// Where a searched file last lost an item of the anchor's symbol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Loss {
    /// In the work tree: `HEAD` wrote it.
    WorkTree,
    /// In this commit: its parent wrote it.
    Commit(String),
}

impl Loss {
    /// The revision before the loss, as git names it.
    fn before(&self) -> String {
        match self {
            Self::WorkTree => "HEAD".to_owned(),
            Self::Commit(commit) => format!("{commit}^"),
        }
    }
}

/// A search and where its file lost the code.
#[derive(Debug)]
pub(crate) struct Lost<'s> {
    search: Search<'s>,
    loss: Loss,
}

/// What git's history must give of `renames` to judge them: each old path
/// before its rename and each new path in the file the rename made.
pub(crate) fn rename_reads(renames: &Renames) -> BTreeSet<(String, String)> {
    let mut reads = BTreeSet::new();
    for (from, renamed) in &renames.to {
        // clone: each read names its revision and path.
        reads.insert((renamed.before.clone(), from.clone()));
        if let Some(made) = &renamed.made {
            // clone: as above.
            reads.insert((made.clone(), renamed.to.clone()));
        }
    }
    reads
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
        match carried(anchor, here.facts, renames, prior) {
            Carried::Moved(moved) => {
                carried_from.insert(anchor.file());
                if asked.stale.contains(anchor) {
                    found.insert(anchor, moved);
                }
            }
            Carried::LeftBehind => {
                refuted.insert(anchor.file());
            }
            Carried::Unshown => {}
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

/// What the rename git reports for a symbol anchor's gone file did to it.
enum Carried {
    /// Carried its code to the anchor on the new path.
    Moved(Anchor),
    /// An inexact rename into a file that did not define what the anchor
    /// resolved to before it: git's pairing is then no move of the file.
    LeftBehind,
    /// Neither is shown.
    Unshown,
}

/// What the rename of `anchor`'s file did to it, a symbol anchor: it
/// carried the code when the definitions before the rename have the span
/// hash they have in the file the rename made, and its symbol resolves at
/// the new path now to definitions of the kinds it named there, which an
/// edit since may have moved out, leaving a declaration of its name.
fn carried(anchor: &Anchor, here: &CodeFacts, renames: &Renames, prior: &Prior) -> Carried {
    let Some(renamed) = renames.to.get(anchor.file()) else {
        return Carried::Unshown;
    };
    let symbol = anchor.symbol().map(str::to_owned);
    // clone: the anchor on the new path owns its path and symbol.
    let Ok(moved) = Anchor::new(renamed.to.clone(), symbol) else {
        return Carried::Unshown;
    };
    let before = prior.resolve(&renamed.before, anchor);
    let (made, kinds) = match &renamed.made {
        Some(commit) => {
            let kinds = prior.whole(commit, &moved).into_iter();
            (
                prior.resolve(commit, &moved),
                kinds.map(|(kind, _)| kind).collect(),
            )
        }
        None => (renames.resolve(here, &moved), renames.kinds(here, &moved)),
    };
    let resolves = matches!(renames.resolve(here, &moved), Resolution::Resolved(_));
    if resolves && before.same_code(&made) && kinds == renames.kinds(here, &moved) {
        return Carried::Moved(moved);
    }
    let held = matches!(before, Resolution::Resolved(_));
    match !renamed.exact && held && !matches!(made, Resolution::Resolved(_)) {
        true => Carried::LeftBehind,
        false => Carried::Unshown,
    }
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

/// The anchor whose file's history the search of `anchor` reads: on the
/// path its file was renamed to when the rename carried the symbol there,
/// so its code left that file later, else `anchor` itself.
fn searched_at(anchor: &Anchor, renames: &Renames, prior: &Prior) -> Option<Anchor> {
    let symbol = anchor.symbol().map(str::to_owned);
    let carried = renames.to.get(anchor.file()).and_then(|renamed| {
        // clone: the anchor on the new path owns its path and symbol.
        let moved = Anchor::new(renamed.to.clone(), symbol.clone()).ok()?;
        let made = renamed.made.as_deref()?;
        prior.wrote(made, &moved).then_some(moved)
    });
    match carried {
        Some(moved) => Some(moved),
        None => Anchor::new(anchor.file().to_owned(), symbol).ok(),
    }
}

/// The stale symbol anchors no rename repairs (`followed`) whose search
/// finds one file, each with it: searched on the anchor's own file, or on
/// the file a rename of it made when the rename carried the symbol.
pub(crate) fn searched<'s>(
    asked: &Asked<'s>,
    here: &Here<'s, '_>,
    renames: &Renames,
    followed: &BTreeMap<&'s Anchor, Anchor>,
    prior: &Prior,
) -> Vec<Search<'s>> {
    let searched: Vec<(&'s Anchor, Anchor)> = asked
        .stale
        .iter()
        .copied()
        .filter(|&anchor| {
            let missing = matches!(
                here.resolutions.get(anchor),
                Some(Resolution::SymbolMissing | Resolution::FileMissing)
            );
            anchor.symbol().is_some() && missing && !followed.contains_key(anchor)
        })
        .filter_map(|anchor| Some((anchor, searched_at(anchor, renames, prior)?)))
        // A macro invocation of the searched file that spells the name may
        // hold the code, which no index sees.
        .filter(|(_, at)| renames.resolve(here.facts, at) != Resolution::SymbolInInvocation)
        .collect();
    let mut namesakes: BTreeMap<Anchor, Namesakes> = here
        .facts
        .namesakes(searched.iter().map(|(_, at)| at))
        .into_iter()
        // clone: the searched anchors move into the searches below.
        .map(|(at, found)| (at.clone(), found))
        .collect();
    searched
        .into_iter()
        .filter_map(|(anchor, at)| {
            let Namesakes { at_home, elsewhere } = namesakes.remove(&at)?;
            let [target] = <[String; 1]>::try_from(elsewhere).ok()?;
            let search = Search { anchor, at, target };
            (!at_home).then_some(search)
        })
        .collect()
}

/// The searched files of `searches` whose loss git's history has not shown
/// yet: those the next read of their commits is for.
pub(crate) fn unjudged<'a>(searches: &'a [Search<'_>], prior: &Prior) -> BTreeSet<&'a str> {
    searches
        .iter()
        .filter(|search| loss(search, prior).is_none())
        .map(|search| search.at.file())
        .collect()
}

/// What git's history must give of each of `searches` to find where its
/// file lost the code, and was not read yet: the file at `HEAD` when the
/// work tree changed it, and before each of its commits known.
pub(crate) fn loss_reads(searches: &[Search<'_>], prior: &Prior) -> BTreeSet<(String, String)> {
    let mut reads = BTreeSet::new();
    for search in searches {
        let file = search.at.file();
        if prior.uncommitted.contains(file) {
            reads.insert((Loss::WorkTree.before(), file.to_owned()));
        }
        for commit in prior.commits.get(file).into_iter().flatten() {
            reads.insert((format!("{commit}^"), file.to_owned()));
        }
    }
    reads.retain(|read| !prior.files.contains_key(read));
    reads
}

/// Where the file of `search` last lost an item of its symbol: the work
/// tree, when the work tree changed it and `HEAD` wrote the item, else the
/// newest of its commits read whose parent wrote it.
fn loss(search: &Search<'_>, prior: &Prior) -> Option<Loss> {
    let at = &search.at;
    if prior.uncommitted.contains(at.file()) && prior.wrote(&Loss::WorkTree.before(), at) {
        return Some(Loss::WorkTree);
    }
    let commits = prior.commits.get(at.file())?;
    let lost = commits
        .iter()
        .find(|commit| prior.wrote(&format!("{commit}^"), at))?;
    // clone: the loss outlives the history it was read from.
    Some(Loss::Commit(lost.clone()))
}

/// Each of `searches` with where its file lost the code, when git's
/// history shows it.
pub(crate) fn lost<'s>(searches: Vec<Search<'s>>, prior: &Prior) -> Vec<Lost<'s>> {
    searches
        .into_iter()
        .filter_map(|search| {
            let loss = loss(&search, prior)?;
            Some(Lost { search, loss })
        })
        .collect()
}

/// What git's history must give of each candidate of `lost` to judge the
/// move: the candidate before the loss, and after it, which for a loss in
/// the work tree is the tree's own.
pub(crate) fn gain_reads(lost: &[Lost<'_>]) -> BTreeSet<(String, String)> {
    let mut reads = BTreeSet::new();
    for Lost { search, loss } in lost {
        // clone: each read names its path.
        reads.insert((loss.before(), search.target.clone()));
        if let Loss::Commit(commit) = loss {
            // clone: as above.
            reads.insert((commit.clone(), search.target.clone()));
        }
    }
    reads
}

/// The candidates of `lost` whose code arrived in a commit: those whose
/// history git is asked for since.
pub(crate) fn targets<'a>(lost: &'a [Lost<'_>]) -> BTreeSet<&'a str> {
    lost.iter()
        .filter(|one| matches!(one.loss, Loss::Commit(_)))
        .map(|one| one.search.target.as_str())
        .collect()
}

/// The repair of each of `lost` that git's history shows moved, by what
/// the tree holds now, `here`.
pub(crate) fn moved<'s>(
    lost: &[Lost<'s>],
    here: &CodeFacts,
    prior: &Prior,
) -> BTreeMap<&'s Anchor, Anchor> {
    lost.iter()
        .filter_map(|one| Some((one.search.anchor, shown_moved(one, here, prior)?)))
        .collect()
}

/// `lost`'s anchor on its candidate, when git's history shows the move in
/// one step: the candidate wrote no item of the symbol before the loss
/// and, after it, holds definitions with the span hash the searched file's
/// had before it; and no later commit deleted or added the candidate, so
/// its file now is the one the code moved into.
fn shown_moved(lost: &Lost<'_>, here: &CodeFacts, prior: &Prior) -> Option<Anchor> {
    let Lost { search, loss } = lost;
    let there = search.there()?;
    let before = loss.before();
    let (after, kept) = match loss {
        Loss::WorkTree => (here.resolve(&there), true),
        Loss::Commit(commit) => (
            prior.resolve(commit, &there),
            prior.kept_since(&search.target, commit),
        ),
    };
    let gained = !prior.wrote(&before, &there) && kept;
    let same = prior.resolve(&before, &search.at).same_code(&after);
    (gained && same).then_some(there)
}
