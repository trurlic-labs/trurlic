//! The repairs of the stale anchors `check` reports: what the calling
//! branch's history says of their files ([`History`]) and the blobs a
//! move is judged by, each read in one bounded read and handed to `lint`
//! as data. Git is asked nothing when no stale anchor's file is gone and no
//! stale symbol anchor's search finds a file, and nothing with no `.git` at
//! or above the code root.
//!
//! A call runs at most six git processes for them, whatever the history:
//! the walk, the diff of what is not committed, and two for each of the two
//! reads of blobs, the anchors' files before their losses, then the
//! candidates around the loss and since. The blobs are indexed through the
//! index's cache, so a warm call indexes none of them again.

use std::collections::{BTreeMap, BTreeSet};

use crate::code::CodeFacts;
use crate::git::renames::at_revision;
use crate::lint::anchors::repair::{self, Search, Shown};
use crate::lint::anchors::{Asked, Here, Prior, Renames, Repair};
use crate::store::{Anchor, Store};

use super::blobs::Blobs;
use super::renames::{Followed, History};
use super::{Reads, read_paths};

/// The newest changes of an anchor's file read for where it lost the
/// code: one that lost it before them gets no repair.
const MAX_LOSS_CHANGES: usize = 16;

/// The changes of a candidate since the code moved into it, each read to
/// see the code kept there: one changed more often gets no repair.
const MAX_SINCE_CHANGES: usize = 128;

/// `asked` with the repair of each stale anchor git's history shows: a
/// move of its gone file's blob, or of its symbol's code to the file that
/// blob reached or to the one file the search finds.
pub(super) fn with_history<'s>(
    store: &Store,
    facts: &CodeFacts,
    here: &Here<'s, '_>,
    asked: Asked<'s>,
    reads: &mut Reads,
) -> Asked<'s> {
    let gone = asked.gone(here);
    let searches = repair::searched(&asked, here, &Renames::none());
    if gone.is_empty() && searches.is_empty() {
        return asked;
    }
    let Some(history) = History::read(store.code_root()) else {
        return asked;
    };
    let followed = history.renamed(&gone);
    let renames = renames(store, facts, &followed, reads);
    let mut repairs = repair::followed(&asked, here, &renames);
    let searches = match followed.is_empty() {
        true => searches,
        false => repair::searched(&asked, here, &renames),
    };
    let judged = Judged {
        history: &history,
        followed: &followed,
        here: facts,
        renames: &renames,
    };
    repairs.extend(judged.moves(store, searches, reads));
    asked.with_repairs(repairs)
}

/// Where each gone file `followed` shows is now, and the facts of those
/// paths beside `here`, the calling tree's, read as an anchor's path is.
fn renames(
    store: &Store,
    here: &CodeFacts,
    followed: &BTreeMap<String, Followed>,
    reads: &mut Reads,
) -> Renames {
    let to: BTreeMap<String, String> = followed
        .iter()
        // clone: the map of paths outlives the history's answer.
        .map(|(gone, followed)| (gone.clone(), followed.to.clone()))
        .collect();
    // clone: the paths are read apart from the map that keeps them.
    let paths = to.values().cloned().collect();
    let facts = here.beside(&read_paths(store, store.code_root(), paths, reads));
    Renames { to, facts }
}

/// Where a file lost an item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Loss {
    /// In the work tree: `HEAD` wrote it.
    WorkTree,
    /// In the commit at this place of the walk: its parent wrote it.
    Commit(usize),
}

/// What the moves of searches are judged by: the history read, where it
/// shows each gone file now, and the facts of the calling tree and of
/// those files.
struct Judged<'j> {
    history: &'j History,
    followed: &'j BTreeMap<String, Followed>,
    here: &'j CodeFacts,
    renames: &'j Renames,
}

impl Judged<'_> {
    /// The repair of each of `searches` git's history shows moved: one on
    /// a gone file by where that file went, any other by where its file
    /// last lost the code, found from the anchor's files before their
    /// newest changes, read in one batch; then its candidate around that
    /// loss and after each change since, read in a second batch.
    fn moves<'s>(
        &self,
        store: &Store,
        searches: Vec<Search<'s>>,
        reads: &mut Reads,
    ) -> BTreeMap<&'s Anchor, Repair> {
        if searches.is_empty() {
            return BTreeMap::new();
        }
        let blobs = Blobs {
            root: store.code_root(),
            store,
        };
        let (on_gone, searched): (Vec<Search<'s>>, Vec<Search<'s>>) = searches
            .into_iter()
            .partition(|search| self.followed.contains_key(search.anchor.file()));
        let carried: Vec<(Search<'s>, Shown)> = on_gone
            .into_iter()
            .filter_map(|search| {
                let shown = carried(self.followed.get(search.anchor.file())?)?;
                Some((search, shown))
            })
            .collect();
        let versions: Vec<Vec<(Loss, String)>> = searched
            .iter()
            .map(|search| versions(self.history, search.anchor.file()))
            .collect();
        let mut prior = Prior::default();
        prior.extend(blobs.read(reads, &losses(&searched, &versions, &carried)));
        let mut shown = self.lost(&prior, searched, versions);
        shown.extend(carried);
        prior.extend(blobs.read(reads, &candidates(&shown)));
        shown
            .iter()
            .filter_map(|(search, shown)| {
                let repair = repair::moved(search, shown, self.now(search), &prior)?;
                Some((search.anchor, repair))
            })
            .collect()
    }

    /// The facts the candidate of `search` is judged by now: the files
    /// gone files went to for one on a gone file, else the calling tree's.
    fn now(&self, search: &Search<'_>) -> &CodeFacts {
        match self.followed.contains_key(search.anchor.file()) {
            true => &self.renames.facts,
            false => self.here,
        }
    }

    /// Each of `searches` with what git shows of its candidate around the
    /// change where its file last lost the code, by its `versions` read
    /// into `prior`; one whose file shows no loss is left out.
    fn lost<'s>(
        &self,
        prior: &Prior,
        searches: Vec<Search<'s>>,
        versions: Vec<Vec<(Loss, String)>>,
    ) -> Vec<(Search<'s>, Shown)> {
        searches
            .into_iter()
            .zip(versions)
            .filter_map(|(search, versions)| {
                let names = versions.iter().map(|(_, name)| name.as_str());
                let at = repair::lost(&search, names, prior)?;
                let (loss, lost) = versions.into_iter().nth(at)?;
                let shown = shown(self.history, loss, lost, search.there.file())?;
                Some((search, shown))
            })
            .collect()
    }
}

/// The file at `path` before each of its newest changes, newest
/// first, where each may have lost an item: before the work tree's, then
/// before each of its [`MAX_LOSS_CHANGES`] newest commits that held it.
fn versions(history: &History, path: &str) -> Vec<(Loss, String)> {
    let uncommitted = history
        .uncommitted(path)
        .and_then(|change| change.before.as_ref());
    // clone: each version names its blob.
    let head = uncommitted.map(|blob| (Loss::WorkTree, blob.clone()));
    let committed = history
        .changes(path)
        .take(MAX_LOSS_CHANGES)
        // clone: as above.
        .filter_map(|(at, change)| Some((Loss::Commit(at), change.before.clone()?)));
    head.into_iter().chain(committed).collect()
}

/// What git's history shows of `target`, a candidate, around `loss`, where
/// the anchor's file, `lost` before it, lost the code: before and after the
/// commit, which must have changed it, and after each of its changes since,
/// none a deletion, at most [`MAX_SINCE_CHANGES`]; for a loss in the work
/// tree, `HEAD`'s file before.
fn shown(history: &History, loss: Loss, lost: String, target: &str) -> Option<Shown> {
    let Loss::Commit(at) = loss else {
        return Some(Shown {
            lost,
            before: Some(at_revision("HEAD", target)),
            after: None,
            since: Vec::new(),
        });
    };
    let (_, change) = history.changes(target).find(|&(commit, _)| commit == at)?;
    let since: Vec<_> = history
        .changes(target)
        .take_while(|&(commit, _)| commit < at)
        .collect();
    if since.len() > MAX_SINCE_CHANGES {
        return None;
    }
    // clone: each read names its blob.
    let since = since.into_iter().map(|(_, change)| change.after.clone());
    Some(Shown {
        lost,
        // clone: as above.
        before: change.before.clone(),
        // clone: as above.
        after: Some(change.after.clone()?),
        since: since.collect::<Option<Vec<String>>>()?,
    })
}

/// What git shows of the candidate of an anchor on a gone file, the file
/// its blob reached: that blob, which left the gone path, on both sides of
/// the move, and the blob after each edit since, at most
/// [`MAX_SINCE_CHANGES`].
fn carried(followed: &Followed) -> Option<Shown> {
    if followed.edits.len() > MAX_SINCE_CHANGES {
        return None;
    }
    Some(Shown {
        // clone: each read names its blob.
        lost: followed.moved.clone(),
        before: None,
        // clone: as above.
        after: Some(followed.moved.clone()),
        // clone: as above.
        since: followed.edits.clone(),
    })
}

/// The reads of the anchors' files before their losses: each of
/// `searched` before each of its `versions`, and each of `carried` before
/// its file went.
fn losses(
    searched: &[Search<'_>],
    versions: &[Vec<(Loss, String)>],
    carried: &[(Search<'_>, Shown)],
) -> BTreeSet<(String, String)> {
    let mut reads = BTreeSet::new();
    for (search, found) in searched.iter().zip(versions) {
        let path = search.anchor.file();
        let named = found.iter().map(|(_, name)| name);
        // clone: each read names its blob and path.
        reads.extend(named.map(|name| (name.clone(), path.to_owned())));
    }
    for (search, shown) in carried {
        // clone: as above.
        reads.insert((shown.lost.clone(), search.anchor.file().to_owned()));
    }
    reads
}

/// The reads of each candidate of `shown`, by its name and the candidate's
/// path.
fn candidates(shown: &[(Search<'_>, Shown)]) -> BTreeSet<(String, String)> {
    let mut reads = BTreeSet::new();
    for (search, shown) in shown {
        let path = search.there.file();
        let names = shown.before.iter().chain(&shown.after).chain(&shown.since);
        // clone: each read names its blob and path.
        reads.extend(names.map(|name| (name.clone(), path.to_owned())));
    }
    reads
}
