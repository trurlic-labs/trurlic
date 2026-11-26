//! The repairs of the stale anchors `check` reports: what the calling
//! branch's history says of their files ([`History`]), read in the steps
//! the repair rules of `lint` ask for, each step's files in one bounded
//! read, and handed to `lint` as data. With no `.git` at or above the code
//! root, git is asked nothing and no anchor is repaired.
//!
//! The git processes a call runs are bounded whatever the history: one
//! diff of what is not committed, one walk per rename hop taken, one for
//! the searched files and their candidates, one diff per commit a rename
//! is judged by, one
//! diff and one listing of what the work tree changed, and two processes
//! for each read of blobs, of which there are at most five.

use std::collections::{BTreeMap, BTreeSet};

use crate::code::CodeFacts;
use crate::lint::anchors::repair::{self, Search};
use crate::lint::anchors::{Asked, Here, Prior, Renames};
use crate::store::{Anchor, Store};

use super::blobs::Blobs;
use super::renames::History;
use super::{Reads, read_paths};

/// The commits of a searched file read for its loss at most, newest
/// first: the newest, then four, then sixteen, each read only for the
/// files whose loss the earlier ones did not show. A file that lost the
/// code before its sixteenth newest change gets no repair.
const MAX_LOSS_COMMITS: usize = 16;

/// `asked` with the repair of each stale anchor git's history shows: a
/// rename of its gone file that carried its code, or a move of its symbol
/// to the one file the search finds.
pub(super) fn with_history<'s>(
    store: &Store,
    facts: &CodeFacts,
    here: &Here<'s, '_>,
    asked: Asked<'s>,
    reads: &mut Reads,
) -> Asked<'s> {
    let root = store.code_root();
    let Some(mut history) = History::of(root) else {
        return asked;
    };
    let renames = renames(store, facts, &mut history, &asked.gone(here), reads);
    let blobs = Blobs {
        root,
        indexer: &reads.indexer,
    };
    let mut prior = Prior::default();
    prior.extend(blobs.read(&repair::rename_reads(&renames)));
    let mut repairs = repair::followed(&asked, here, &renames, &prior);
    let searches = repair::searched(&asked, here, &renames, &repairs, &prior);
    repairs.extend(moves(&mut history, &blobs, facts, searches, prior));
    asked.with_repairs(repairs)
}

/// Where git says each of `gone`, the files of stale anchors with nothing
/// at their path, was renamed to, and the facts of those paths beside
/// `here`, the calling tree's, read as an anchor's path is.
fn renames(
    store: &Store,
    here: &CodeFacts,
    history: &mut History<'_>,
    gone: &BTreeSet<&str>,
    reads: &mut Reads,
) -> Renames {
    let to = history.renamed(gone);
    if to.is_empty() {
        return Renames::none();
    }
    // clone: the paths are read apart from the map that keeps them.
    let paths = to.values().map(|renamed| renamed.to.clone()).collect();
    let facts = here.beside(&read_paths(store, store.code_root(), paths, reads));
    Renames { to, facts }
}

/// The repair of each of `searches` git's history shows moved, by the
/// calling tree's facts `here`: where each searched file last lost the
/// code, read before its newest commits in up to three rounds, the
/// candidate before and after that loss, and the commits that changed the
/// candidate.
fn moves<'s>(
    history: &mut History<'_>,
    blobs: &Blobs<'_>,
    here: &CodeFacts,
    searches: Vec<Search<'s>>,
    mut prior: Prior,
) -> BTreeMap<&'s Anchor, Anchor> {
    if searches.is_empty() {
        return BTreeMap::new();
    }
    let files: BTreeSet<&str> = searches.iter().map(|search| search.at.file()).collect();
    // The candidates are walked with the searched files, in the one walk.
    let walked = files
        .iter()
        .copied()
        .chain(searches.iter().map(|search| search.target.as_str()))
        .collect();
    let commits = history.commits(&walked);
    let uncommitted = history.uncommitted(&files);
    let (mut wanted, mut take) = (files, 1);
    loop {
        for &file in &wanted {
            let read = commits.get(file).map_or(&[][..], Vec::as_slice);
            // clone: the history keeps each commit it has read.
            let read = read.iter().take(take).map(|c| c.commit.clone()).collect();
            prior.changed(file.to_owned(), read, uncommitted.contains(file));
        }
        prior.extend(blobs.read(&repair::loss_reads(&searches, &prior)));
        if take >= MAX_LOSS_COMMITS {
            break;
        }
        let more = |file: &&str| commits.get(*file).is_some_and(|all| all.len() > take);
        wanted = repair::unjudged(&searches, &prior);
        wanted.retain(more);
        if wanted.is_empty() {
            break;
        }
        take *= 4;
    }
    let lost = repair::lost(searches, &prior);
    prior.extend(blobs.read(&repair::gain_reads(&lost)));
    for (target, changes) in history.commits(&repair::targets(&lost)) {
        let changes = changes.into_iter().map(|c| (c.commit, c.replaced));
        prior.target_changed(target, changes.collect());
    }
    repair::moved(&lost, here, &prior)
}
