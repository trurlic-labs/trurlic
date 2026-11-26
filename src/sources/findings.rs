//! The anchor findings of `check` and `status`: the decisions that hold an
//! anchor, judged in the calling tree, and for `check` in the repository's
//! other worktrees and git's history. What each tree holds is read through
//! one [`Reads`] (`reads.rs`).

use std::collections::{BTreeMap, BTreeSet};

use crate::code::{CodeFacts, Entry};
use crate::git::renames::at_revision;
use crate::lint::anchors::{self, Asked, Checked, Decisions, Elsewhere, Finding, Here, Unasked};
use crate::store::{Anchor, Decision, Store};

use super::blobs::Blobs;
use super::reads::{Reads, Scope, read_paths, read_tree};
use super::repair;
use super::worktrees::{self, UnreadWorktree};

/// The anchor findings `check` reports, and the other worktrees git
/// could not say enough of to read.
#[derive(Debug, Default)]
pub(crate) struct CheckedFindings<'s> {
    pub(crate) findings: Vec<Finding<'s, Checked>>,
    pub(crate) unread: Vec<UnreadWorktree>,
    /// The commit `HEAD` named when git's history was read for repairs.
    pub(crate) head: Option<String>,
}

/// The anchor findings of `decisions` that `check` reports, judged in the
/// tree at `store`'s code root and, for an anchor of a decision ever
/// accepted that is missing there, in the repository's other worktrees
/// that changed its file since their line forked from the calling tree's.
/// Decisions with no anchor read no file.
pub(crate) fn checked_findings<'s>(
    store: &Store,
    decisions: impl IntoIterator<Item = (&'s String, &'s Decision)>,
) -> CheckedFindings<'s> {
    let decisions = anchored(decisions);
    if decisions.is_empty() {
        return CheckedFindings::default();
    }
    let mut reads = Reads::of_tree(store);
    let facts = calling_tree(store, &decisions, &mut reads);
    let here = Here::of(&decisions, &facts);
    let mut asked = Asked::new(here.unresolved(&decisions));
    let unread = elsewhere(store, &facts, &mut asked, &mut reads);
    let (asked, head) = repair::with_history(store, &facts, &here, asked, &mut reads);
    reads.write(store);
    CheckedFindings {
        findings: anchors::findings(&decisions, &here, &asked),
        unread,
        head,
    }
}

/// The anchor findings of `decisions` that `status` reports, judged in
/// the tree at `store`'s code root alone: the project's git is asked
/// nothing. Decisions with no anchor read no file.
pub(crate) fn tree_findings<'s>(
    store: &Store,
    decisions: impl IntoIterator<Item = (&'s String, &'s Decision)>,
) -> Vec<Finding<'s, Unasked>> {
    let decisions = anchored(decisions);
    if decisions.is_empty() {
        return Vec::new();
    }
    let mut reads = Reads::of_tree(store);
    let facts = calling_tree(store, &decisions, &mut reads);
    reads.write(store);
    anchors::unasked(&decisions, &Here::of(&decisions, &facts))
}

/// The decisions of `decisions` that hold an anchor, by name.
fn anchored<'s>(
    decisions: impl IntoIterator<Item = (&'s String, &'s Decision)>,
) -> Vec<(&'s str, &'s Decision)> {
    decisions
        .into_iter()
        .map(|(name, decision)| (name.as_str(), decision))
        .filter(|(_, decision)| !decision.anchors.is_empty())
        .collect()
}

/// The facts of the calling tree: every source the walk takes, and the
/// paths `decisions` anchor.
fn calling_tree(store: &Store, decisions: &Decisions<'_>, reads: &mut Reads) -> CodeFacts {
    let anchored = decisions
        .iter()
        .flat_map(|(_, decision)| &decision.anchors)
        .map(Anchor::file);
    let (entries, walked) = read_tree(store, store.code_root(), Scope::Tree, anchored, reads);
    CodeFacts::new(entries, walked)
}

/// Read the other worktrees into `asked` for its stale anchors, those of
/// decisions ever accepted missing in the calling tree, whose facts `here`
/// are, and give those git could not say enough of; none is asked when
/// nothing is missing. Each worktree, in git's order, is read for the
/// anchors no earlier one holds at the paths its work tree changed since
/// the fork, and no further worktree is read once every one is held. Those
/// files alone are read, never a walk of the tree, against the calling
/// tree's crates and module declarations, there and as the fork held them,
/// and its blank set.
fn elsewhere(
    store: &Store,
    here: &CodeFacts,
    asked: &mut Asked<'_>,
    reads: &mut Reads,
) -> Vec<UnreadWorktree> {
    if asked.stale().is_empty() {
        return Vec::new();
    }
    let others = worktrees::others(store.code_root());
    let mut unread = others.unread;
    for forked in others.trees {
        if asked.stale().is_empty() {
            break;
        }
        let paths: BTreeSet<&str> = asked.stale().iter().map(|&anchor| anchor.file()).collect();
        let paths: Vec<&str> = paths.into_iter().collect();
        let changed = match forked.changed(&paths) {
            Ok(changed) if changed.is_empty() => continue,
            Ok(changed) => changed,
            Err(error) => {
                unread.push(UnreadWorktree {
                    worktree: Some(forked.top),
                    cause: error.into(),
                });
                continue;
            }
        };
        let fork = forked.base().map(|base| {
            let blobs = Blobs {
                root: &forked.top,
                store,
            };
            at_fork(here, &blobs, reads, base, &changed)
        });
        let facts = here.beside(&read_paths(store, &forked.top, changed, reads));
        asked.read(Elsewhere {
            worktree: forked.worktree,
            facts,
            fork,
        });
    }
    unread
}

/// The facts of `paths` at commit `base`, read from git beside `here`, the
/// calling tree's.
fn at_fork(
    here: &CodeFacts,
    blobs: &Blobs<'_>,
    reads: &mut Reads,
    base: &str,
    paths: &BTreeSet<String>,
) -> CodeFacts {
    let asked = paths
        .iter()
        // clone: each read names its blob and path.
        .map(|path| (at_revision(base, path), path.clone()))
        .collect();
    // Every path holds nothing at the fork until git gives its blob, so the
    // calling tree's file there is masked either way.
    let mut read: BTreeMap<String, Option<Entry>> = paths
        .iter()
        // clone: the facts at the fork key each path the calling tree's do.
        .map(|path| (path.clone(), None))
        .collect();
    for (_, path, entry) in blobs.read(reads, &asked) {
        read.insert(path, Some(entry));
    }
    here.beside(&read)
}
