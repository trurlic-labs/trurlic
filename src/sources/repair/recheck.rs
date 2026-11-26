//! The recheck of the repairs a read judged, under the write lock
//! ([`still_holding`]): each held to the tree again, through the same
//! `Reads` and with no git but `HEAD`.

use std::collections::{BTreeMap, BTreeSet};

use crate::code::{Code, CodeFacts, Resolution};
use crate::git;
use crate::store::{Anchor, Store};

use super::super::reads::{Reads, Scope, read_tree};

/// What a read of the repairs saw, which the write holds them to again
/// under its lock: the code each repair named, by its stale anchor, and
/// the commit `HEAD` named when git's history was read, `None` when it was
/// not.
#[derive(Debug, Default)]
pub(crate) struct Seen {
    pub(crate) code: BTreeMap<Anchor, Code>,
    pub(crate) head: Option<String>,
}

/// Keep of `repairs`, a map from stale anchor to repair read before the
/// write lock, each that still holds in the tree at `store`'s code root,
/// read under the lock from the files the repairs name alone, as the tree
/// read judges them, with no git: its stale anchor names nothing there and
/// its repair names the code `seen` holds for it, what the read judged, so
/// code replaced since is never written; and none when `HEAD` names
/// another commit than the read's history walk did, a branch switched or a
/// commit made between them. Give each repair dropped, by its stale
/// anchor, with why.
pub(crate) fn still_holding(
    store: &Store,
    repairs: &mut BTreeMap<Anchor, Anchor>,
    seen: &Seen,
) -> BTreeMap<Anchor, String> {
    if repairs.is_empty() {
        return BTreeMap::new();
    }
    let moved = seen.head.as_ref().is_some_and(|head| {
        // A git that cannot say counts as a move: the repairs wait.
        !git::head(store.code_root()).is_ok_and(|now| now == *head)
    });
    if moved {
        let cause = "the branch moved since the repairs were read; run `repair_anchors` again";
        // clone: the map dropped outlives the one cleared.
        let dropped = repairs.keys().map(|from| (from.clone(), cause.to_owned()));
        let dropped = dropped.collect();
        repairs.clear();
        return dropped;
    }
    let paths: BTreeSet<&str> = repairs
        .iter()
        .flat_map(|(from, to)| [from.file(), to.file()])
        .collect();
    let facts = read_alone(store, paths);
    let mut dropped = BTreeMap::new();
    repairs.retain(|from, to| {
        let stale = matches!(
            facts.resolve(from),
            Resolution::FileMissing | Resolution::SymbolMissing
        );
        let same = seen
            .code
            .get(from)
            .is_some_and(|seen| facts.code(to).as_ref() == Some(seen));
        let holds = stale && same;
        if !holds {
            let cause = "its code is back, or the code its repair names changed, since the \
                         repairs were read; run `repair_anchors` again";
            // clone: the map dropped outlives the one kept.
            dropped.insert(from.clone(), cause.to_owned());
        }
        holds
    });
    dropped
}

/// The facts of `paths` in the tree at `store`'s code root, read through
/// the reads of the tree, with no other file of it; the cache is read, not
/// written, since the caller holds the write lock.
fn read_alone(store: &Store, paths: BTreeSet<&str>) -> CodeFacts {
    let mut reads = Reads::of_tree(store);
    let root = store.code_root();
    let (entries, _) = read_tree(store, root, Scope::Anchored, paths.into_iter(), &mut reads);
    CodeFacts::new(entries, BTreeSet::new())
}
