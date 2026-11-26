//! The repairs of the stale anchors `check` reports: what the calling
//! branch's history says of their files ([`History`]), read in the steps
//! the repair rules of `lint` ask for, each step's files in one bounded
//! read, and handed to `lint` as data. With no `.git` at or above the code
//! root, git is asked nothing and no anchor is repaired.

use std::collections::{BTreeMap, BTreeSet};

use crate::code::CodeFacts;
use crate::lint::anchors::repair::{self, Search};
use crate::lint::anchors::{Asked, Here, Judged, Prior, Renames};
use crate::store::{Anchor, Store};

use super::blobs::Blobs;
use super::renames::History;
use super::{Reads, read_paths};

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
    let before: BTreeSet<(String, String)> = renames
        .to
        .iter()
        // clone: the reads are keyed by revision and path apart from the map.
        .map(|(from, renamed)| (renamed.before.clone(), from.clone()))
        .collect();
    let blobs = Blobs {
        root,
        indexer: &reads.indexer,
    };
    let mut prior = Prior::default();
    prior.extend(blobs.read(&before));
    let mut repairs = repair::followed(&asked, here, &renames, &prior);
    let searches = repair::searched(&asked, here, &renames, &repairs);
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
/// calling tree's facts `here`: where each anchor's file last held its
/// code, the commits since that changed how often its candidate spells the
/// symbol's name, those files there, and, for a candidate that gained it
/// later, the files that may have carried the code between.
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
    let files: BTreeSet<&str> = searches.iter().map(|search| search.anchor.file()).collect();
    for (file, judged) in history.judged(&files) {
        prior.judge_at(file, judged);
    }
    for search in &searches {
        let Some(Judged::Before(lost)) = prior.judged(search.anchor.file()) else {
            continue;
        };
        let since = format!("{lost}^");
        let commits = history.spelling_changes(&since, &search.target, &search.name);
        prior.spelled(search.anchor.file(), &search.target, &search.name, commits);
    }
    prior.extend(blobs.read(&repair::gain_reads(&searches, &prior)));
    let gained = repair::gained(searches, &prior);
    let changed = history.changed_by(&repair::later_commits(&gained, &prior));
    for (commit, paths) in changed {
        prior.changed(commit, paths);
    }
    prior.extend(blobs.read(&repair::carrier_reads(&gained, &prior)));
    repair::moved(&gained, here, &prior)
}
