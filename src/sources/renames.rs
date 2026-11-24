//! Where git says an anchored file that is gone went, and the revision
//! where an anchored file last held its code.
//!
//! Asked only for `check` and the `repair_anchors` op, for the files of
//! stale anchors, and only when a `.git` is at or above the code root.
//! What is not committed comes first: a rename between `HEAD` and the work
//! tree or the index, as `git mv` leaves it (a file moved without git and
//! not yet added is no rename git can see). Else the calling branch's
//! history: the most recent commit that changed the path, which for a
//! path gone from `HEAD` deleted it, and what that commit renamed it to. A
//! path renamed to one that is gone too is followed from there, at most
//! [`MAX_RENAME_HOPS`] times; one walk of the history serves every path of
//! a hop, run only for a hop that is taken, and each commit is diffed
//! once. A path renamed to several, or copied beside the rename, was
//! split: no answer. The answer is exact when every rename on the way has
//! git's similarity 100, and names the revision before the first. A
//! failure of git leaves the paths it concerns without an answer, which
//! leaves their anchors stale without a repair, as they are.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::Result;
use crate::git::renames::{Changes, Moved, last_commits, moved};
use crate::git::{self, DotGit};
use crate::lint::anchors::Renamed;

/// The renames followed from one gone path before the answer is given up.
pub(crate) const MAX_RENAME_HOPS: usize = 8;

/// For each of `gone`, paths under `code_root` with nothing there, the path
/// git renamed it to that the tree holds now, when git says exactly one.
pub(super) fn renamed(code_root: &Path, gone: &BTreeSet<&str>) -> BTreeMap<String, Renamed> {
    let in_git = git::dot_git(code_root).is_ok_and(|dot_git| dot_git != DotGit::Absent);
    if gone.is_empty() || !in_git {
        return BTreeMap::new();
    }
    let mut history = History::new(code_root);
    let mut found = BTreeMap::new();
    let mut asked: BTreeMap<String, Option<Renamed>> =
        gone.iter().map(|&path| (path.to_owned(), None)).collect();
    for _ in 0..MAX_RENAME_HOPS {
        let paths: Vec<String> = asked
            .iter()
            .map(|(start, r)| at(start, r.as_ref()))
            .collect();
        history.look_up(paths.iter().map(String::as_str));
        let mut next = BTreeMap::new();
        for (start, so_far) in asked {
            let Some(hop) = history.renamed(&at(&start, so_far.as_ref())) else {
                continue;
            };
            let exact = so_far.as_ref().is_none_or(|r| r.exact) && hop.similarity == EXACT;
            let before = so_far.map_or(hop.before, |r| r.before);
            let there = code_root.join(&hop.to).exists();
            let renamed = Renamed {
                to: hop.to,
                exact,
                before,
            };
            if there {
                found.insert(start, renamed);
            } else {
                next.insert(start, Some(renamed));
            }
        }
        if next.is_empty() {
            break;
        }
        asked = next;
    }
    found
}

/// The path a gone path `start` has reached: where its renames so far
/// took it, or itself before the first.
fn at(start: &str, so_far: Option<&Renamed>) -> String {
    // clone: the next hop reads the path the last one reached.
    so_far.map_or_else(|| start.to_owned(), |renamed| renamed.to.clone())
}

/// git's similarity of a rename that kept the content whole.
const EXACT: u8 = 100;

/// One rename git reports of a path: where it went, git's similarity, and
/// the revision before it.
struct Hop {
    to: String,
    similarity: u8,
    before: String,
}

/// For each of `files`, anchored paths under `code_root`, the revision
/// where it last held what the tree no longer does: `HEAD` when the work
/// tree changed it since, else the parent of the last commit that changed
/// it (for a file gone from `HEAD`, the one that deleted it), one walk for
/// them all. A file git gives nothing of is left out.
pub(super) fn revisions(code_root: &Path, files: &BTreeSet<&str>) -> BTreeMap<String, String> {
    let in_git = git::dot_git(code_root).is_ok_and(|dot_git| dot_git != DotGit::Absent);
    if files.is_empty() || !in_git {
        return BTreeMap::new();
    }
    let paths: Vec<&str> = files.iter().copied().collect();
    let Ok(changed) = git::changed_since(code_root, "HEAD", &paths) else {
        return BTreeMap::new();
    };
    let committed: Vec<&str> = paths
        .into_iter()
        .filter(|path| !changed.contains(*path))
        .collect();
    let last = match committed.is_empty() {
        true => BTreeMap::new(),
        false => last_commits(code_root, &committed).unwrap_or_default(),
    };
    let head = changed.into_iter().map(|path| (path, "HEAD".to_owned()));
    let parents = last
        .into_iter()
        .map(|(path, commit)| (path, format!("{commit}^")));
    head.chain(parents).collect()
}

/// What git said so far: the uncommitted renames, the commit that last
/// changed (so deleted) each gone path asked, and the renames of each such
/// commit.
struct History<'r> {
    root: &'r Path,
    uncommitted: Vec<Moved>,
    deleted_by: BTreeMap<String, String>,
    by_commit: BTreeMap<String, Vec<Moved>>,
}

impl<'r> History<'r> {
    fn new(root: &'r Path) -> Self {
        Self {
            root,
            uncommitted: moved(root, Changes::Uncommitted).unwrap_or_default(),
            deleted_by: BTreeMap::new(),
            by_commit: BTreeMap::new(),
        }
    }

    /// Ask, in one walk, which commit last changed, so deleted, each of
    /// `paths`.
    fn look_up<'p>(&mut self, paths: impl Iterator<Item = &'p str>) {
        let paths: Vec<&str> = paths
            .filter(|path| !self.deleted_by.contains_key(*path))
            .collect();
        if paths.is_empty() {
            return;
        }
        #[cfg(test)]
        WALKS.with(|walks| walks.set(walks.get() + 1));
        if let Ok(deleted) = last_commits(self.root, &paths) {
            self.deleted_by.extend(deleted);
        }
    }

    /// The one path `path` was renamed to, not committed or in the commit
    /// that last deleted it, which [`look_up`](Self::look_up) found; `None`
    /// when git names none, a deletion, or several paths (a split shows as
    /// a copy beside the rename).
    fn renamed(&mut self, path: &str) -> Option<Hop> {
        if let Some(hop) = only_rename(&self.uncommitted, path) {
            let (to, similarity) = hop?;
            let before = "HEAD".to_owned();
            return Some(Hop {
                to,
                similarity,
                before,
            });
        }
        let commit = self.deleted_by.get(path)?.to_owned();
        let before = format!("{commit}^");
        let changes = match self.by_commit.entry(commit) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => {
                let commit = entry.key().as_str();
                let read: Result<Vec<Moved>> = moved(self.root, Changes::Commit(commit));
                entry.insert(read.unwrap_or_default())
            }
        };
        let (to, similarity) = only_rename(changes, path).flatten()?;
        Some(Hop {
            to,
            similarity,
            before,
        })
    }
}

/// What `changes` made of `path`: `None` when nothing, `Some(None)` when a
/// deletion or several records, `Some(Some((to, similarity)))` for one
/// rename. A split shows as two records, a copy beside the rename.
fn only_rename(changes: &[Moved], path: &str) -> Option<Option<(String, u8)>> {
    let mut from = changes.iter().filter(|change| change.from == path);
    let first = from.next()?;
    let one = from.next().is_none();
    // clone: the answer outlives the diff it was read from.
    let to = first.to.clone().filter(|_| one);
    Some(to.map(|to| (to, first.similarity)))
}

// The history walks a test counts, on the current thread.
#[cfg(test)]
thread_local! {
    static WALKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
mod tests;
