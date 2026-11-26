//! The repairs of the stale anchors `check` reports: what the calling
//! branch's history says of their files ([`History`]) and the blobs a
//! move is judged by, each read in one bounded read and handed to `lint`
//! as data. Git is asked nothing when no stale anchor's file is gone or
//! defines nothing its symbol names, and nothing with no `.git` at or
//! above the code root.
//!
//! A call runs at most six git processes for them, whatever the history:
//! the walk, the diff of what is not committed, and two for each of the two
//! reads of blobs, the anchors' files before their newest changes, then
//! the files each loss changed around it and since. The blobs are indexed
//! through the index's cache, so a warm call indexes none of them again.

use std::collections::{BTreeMap, BTreeSet};

use crate::code::CodeFacts;
use crate::git::renames::Change;
use crate::lint::anchors::repair::{self, Search, Shown};
use crate::lint::anchors::{Asked, Here, Prior, Renames, Repair};
use crate::store::{Anchor, Store};

use super::blobs::Blobs;
use super::renames::History;
use super::{Reads, read_paths};

/// The newest changes of an anchor's file read for where it lost the
/// code: one that lost it before them gets no repair.
const MAX_LOSS_CHANGES: usize = 8;

/// The changes of a candidate since the code moved into it, each read to
/// see the code kept there: a loss with a candidate changed more often gets
/// no repair, since the move cannot be judged among all of them.
const MAX_SINCE_CHANGES: usize = 128;

/// The files one loss changed that are judged as where its code went:
/// those of the anchor's language that write its symbol now. A loss with
/// more gets no repair rather than a judgement among some of them, which
/// could pick the wrong one of two; each is two blobs and its changes
/// since, so a commit of thousands of files is never read whole.
const MAX_LOSS_CANDIDATES: usize = 16;

/// `asked` with the repair of each stale anchor git's history shows: a
/// move of its gone file's blob, or of its symbol's code into the one file
/// its loss changed that shows it; and the commit `HEAD` named when the
/// history was read, `None` when it was not.
pub(super) fn with_history<'s>(
    store: &Store,
    facts: &CodeFacts,
    here: &Here<'s, '_>,
    asked: Asked<'s>,
    reads: &mut Reads,
) -> (Asked<'s>, Option<String>) {
    let gone = asked.gone(here);
    let missing = repair::missing(&asked, here);
    if gone.is_empty() && missing.is_empty() {
        return (asked, None);
    }
    let Some(history) = History::read(store.code_root()) else {
        return (asked, None);
    };
    let to = history.renamed(&gone);
    // clone: the paths are read apart from the map that keeps them.
    let paths = to.values().cloned().collect();
    let read = facts.beside(&read_paths(store, store.code_root(), paths, reads));
    let mut repairs = repair::followed(&asked, here, &Renames { to, facts: read });
    repairs.extend(moves(store, &history, facts, &missing, reads));
    // clone: the head is named past the history read.
    (
        asked.with_repairs(repairs),
        history.head().map(str::to_owned),
    )
}

/// Where a file lost an item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Loss {
    /// In the work tree: `HEAD` wrote it.
    WorkTree,
    /// In the commit at this place of the walk: its parent wrote it.
    Commit(usize),
}

/// The repair of each of `missing`, with the files that write its symbol
/// now, that git's history shows moved, by the calling tree's facts
/// `here`: where each anchor's file last lost the code, found from its
/// files before its newest changes, read in one batch; then the files
/// that loss changed that are now among those, around it and after each
/// change since, read in a second batch.
fn moves<'s>(
    store: &Store,
    history: &History,
    here: &CodeFacts,
    missing: &BTreeMap<&'s Anchor, BTreeSet<&str>>,
    reads: &mut Reads,
) -> BTreeMap<&'s Anchor, Repair> {
    let blobs = Blobs {
        root: store.code_root(),
        store,
    };
    let writers: BTreeSet<&str> = missing.values().flatten().copied().collect();
    let lineages = Lineages {
        history,
        of: writers
            .into_iter()
            .map(|path| (path, history.lineage(path)))
            .collect(),
    };
    let versions: Vec<Searched<'s, '_>> = missing
        .iter()
        .filter_map(|(&anchor, writers)| lineages.searched(anchor, writers))
        .collect();
    let mut prior = Prior::default();
    prior.extend(blobs.read(reads, &losses(&versions)));
    let judged: Vec<(&'s Anchor, Vec<(Search<'s>, Shown)>)> = versions
        .into_iter()
        .filter_map(|(anchor, writers, versions)| {
            let sides = versions
                .iter()
                .map(|v| (v.before.as_str(), v.after.as_deref()));
            let at = repair::lost(anchor, sides, &prior)?;
            let Version { loss, before, .. } = versions.into_iter().nth(at)?;
            Some((
                anchor,
                lineages.candidates((anchor, loss, &before), writers)?,
            ))
        })
        .collect();
    // The versions read for the losses but the losses themselves are done
    // with: their indexes are dropped before the second read.
    let lost = judged.iter().flat_map(|(anchor, searches)| {
        // clone: each file kept is named by its blob and path.
        let names = searches.iter().map(|(_, shown)| shown.lost.clone());
        names.map(|name| (name, anchor.file().to_owned()))
    });
    prior.retain(&lost.collect());
    prior.extend(blobs.read(reads, &around(&judged)));
    judged
        .iter()
        .filter_map(|(anchor, searches)| Some((*anchor, repair::moved_to(searches, here, &prior)?)))
        .collect()
}

/// An anchor whose code is looked for, the files that write its symbol
/// now, and its file around each of its newest changes that may hold the
/// loss.
type Searched<'s, 'w> = (&'s Anchor, &'w BTreeSet<&'w str>, Vec<Version>);

/// A change of an anchor's file that may have lost its code: where it was
/// made, and the file's blob before it and after it, `None` after a
/// deletion or for the work tree's file, which writes nothing the anchor
/// names.
struct Version {
    loss: Loss,
    before: String,
    after: Option<String>,
}

/// The history, and the lineage of each file that writes a missing
/// anchor's symbol now ([`History::lineage`]).
struct Lineages<'h, 'f> {
    history: &'h History,
    of: BTreeMap<&'f str, BTreeMap<usize, String>>,
}

impl Lineages<'_, '_> {
    /// `anchor` with its versions, up to the oldest change that changed a
    /// file of the lineage of one of `writers`, the files that write its
    /// symbol now: a loss older than it can show no move; `None` when no
    /// change does.
    fn searched<'s, 'w>(
        &self,
        anchor: &'s Anchor,
        writers: &'w BTreeSet<&'w str>,
    ) -> Option<Searched<'s, 'w>> {
        let mut found = versions(self.history, anchor.file());
        let reaches = |loss: Loss| {
            let mut other = writers.iter().filter_map(|writer| self.at(writer, loss));
            other.any(|change| change.path != anchor.file())
        };
        let last = found.iter().rposition(|version| reaches(version.loss))?;
        found.truncate(last + 1);
        Some((anchor, writers, found))
    }

    /// The change `loss` made to the file of `writer`'s lineage there, one
    /// that left a file at the path.
    fn at(&self, writer: &str, loss: Loss) -> Option<&Change> {
        let change = match loss {
            Loss::Commit(at) => {
                let path = self.of.get(writer)?.get(&at)?;
                let mut made = self.history.made(at).iter();
                made.find(|change| change.path == *path)?
            }
            Loss::WorkTree => self.history.uncommitted(writer)?,
        };
        (change.status != b'D').then_some(change)
    }

    /// The candidates of `anchor`, whose file was `lost` before `loss`: the
    /// files of `writers` whose lineage `loss` changed at another path,
    /// followed from that change to them, each with what git shows of it
    /// around the loss. `None` past [`MAX_LOSS_CANDIDATES`], or when one
    /// changed more than [`MAX_SINCE_CHANGES`] times since.
    fn candidates<'s>(
        &self,
        (anchor, loss, lost): (&'s Anchor, Loss, &str),
        writers: &BTreeSet<&str>,
    ) -> Option<Vec<(Search<'s>, Shown)>> {
        let mut found = Vec::new();
        for writer in writers {
            let Some(change) = self.at(writer, loss).filter(|c| c.path != anchor.file()) else {
                continue;
            };
            let Some((to, since)) = followed(self.history, loss, change) else {
                continue;
            };
            if to != *writer {
                continue;
            }
            if since.len() > MAX_SINCE_CHANGES || found.len() == MAX_LOSS_CANDIDATES {
                return None;
            }
            let there = Anchor::new(to, anchor.symbol().map(str::to_owned)).ok()?;
            let shown = Shown {
                lost: lost.to_owned(),
                // clone: each read names its blob.
                before: change.before.clone(),
                // clone: as above; a work tree's file is the tree's.
                after: change.after.clone().filter(|_| loss != Loss::WorkTree),
                since,
            };
            found.push((Search { anchor, there }, shown));
        }
        Some(found)
    }
}

/// The file at `path` around each of its newest changes, newest first,
/// where each may have lost an item: the work tree's, then each of its
/// [`MAX_LOSS_CHANGES`] newest commits that held it before.
fn versions(history: &History, path: &str) -> Vec<Version> {
    let version = |loss: Loss, change: &Change| {
        Some(Version {
            loss,
            // clone: each version names its blobs.
            before: change.before.clone()?,
            // clone: as above.
            after: change.after.clone().filter(|_| loss != Loss::WorkTree),
        })
    };
    let head = history
        .uncommitted(path)
        .and_then(|change| version(Loss::WorkTree, change));
    let committed = history.changes(path).take(MAX_LOSS_CHANGES);
    let committed = committed.filter_map(|(at, change)| version(Loss::Commit(at), change));
    head.into_iter().chain(committed).collect()
}

/// Where the file `change`, one of `loss`'s, is now, and the blob after
/// each edit since; the file itself for a loss in the work tree.
fn followed(history: &History, loss: Loss, change: &Change) -> Option<(String, Vec<String>)> {
    match loss {
        // clone: the blob followed changes as the file moves.
        Loss::Commit(at) => history.kept(change.path.clone(), change.after.clone()?, at),
        // clone: the file is named by its path.
        Loss::WorkTree => Some((change.path.clone(), Vec::new())),
    }
}

/// The reads of the anchors' files around their losses: each of
/// `searched` before and after each of its versions.
fn losses(searched: &[Searched<'_, '_>]) -> BTreeSet<(String, String)> {
    let mut reads = BTreeSet::new();
    for (anchor, _, found) in searched {
        let named = found
            .iter()
            .flat_map(|v| std::iter::once(&v.before).chain(&v.after));
        // clone: each read names its blob and path.
        reads.extend(named.map(|name| (name.clone(), anchor.file().to_owned())));
    }
    reads
}

/// The reads of each candidate of `judged` around its loss, by its name
/// and the candidate's path.
fn around(judged: &[(&Anchor, Vec<(Search<'_>, Shown)>)]) -> BTreeSet<(String, String)> {
    let mut reads = BTreeSet::new();
    for (search, shown) in judged.iter().flat_map(|(_, searches)| searches) {
        let path = search.there.file();
        let names = shown.before.iter().chain(&shown.after).chain(&shown.since);
        // clone: each read names its blob and path.
        reads.extend(names.map(|name| (name.clone(), path.to_owned())));
    }
    reads
}
