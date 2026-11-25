//! What the calling branch's history says of the files of stale anchors:
//! where a gone file went, and where an anchored file last held its code.
//!
//! The history is read only for `check` and the `repair_anchors` op, and
//! only when a `.git` is at or above the code root ([`History::of`]). What is not committed
//! comes first: a rename between `HEAD` and the work tree or the index, as
//! `git mv` leaves it (a file moved without git and not yet added is no
//! rename git can see). Else the calling branch's history: the most recent
//! commit that changed the path, which for a path gone from `HEAD` deleted
//! it, and what that commit renamed it to. A committed rename counts only
//! while its new path is the file it made: the newest commit that added
//! that path is the rename's own, so a path deleted and written again since
//! is no answer. A path renamed to one that is gone too is followed from
//! there, at most [`MAX_RENAME_HOPS`] times; one walk of the history serves
//! every path of a hop, and the paths a hop reached, and each commit is
//! diffed once. A path renamed to several, or copied beside the rename, was
//! split: no answer. The answer is exact when every rename on the way has
//! git's similarity 100, and names the revision before the first. A
//! failure of git leaves the paths it concerns without an answer, which
//! leaves their anchors stale without a repair, as they are.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::Result;
use crate::git::renames::{Changed, Changes, Moved, changed_by, last_changes, moved};
use crate::git::{self, DotGit};
use crate::lint::anchors::{Judged, Renamed};

/// The renames followed from one gone path before the answer is given up.
pub(crate) const MAX_RENAME_HOPS: usize = 8;

/// git's similarity of a rename that kept the content whole.
const EXACT: u8 = 100;

/// What git said so far of the repository holding the code root: the
/// uncommitted renames, once asked, what the history says of each path a
/// walk asked about, and the renames of each commit diffed.
pub(super) struct History<'r> {
    root: &'r Path,
    uncommitted: Option<Vec<Moved>>,
    walked: BTreeSet<String>,
    changes: BTreeMap<String, Changed>,
    by_commit: BTreeMap<String, Vec<Moved>>,
}

/// One rename git reports of a path: where it went, git's similarity, and
/// the commit that made it, `None` for one not committed.
struct Hop {
    to: String,
    similarity: u8,
    commit: Option<String>,
}

/// Where a gone path's renames took it so far, and the commit of the last
/// rename, whose new path is checked before it counts.
struct Reached {
    renamed: Renamed,
    commit: Option<String>,
}

impl<'r> History<'r> {
    /// The history of the repository holding `root`, the code root; `None`
    /// when no `.git` is at or above it, so git is asked nothing.
    pub(super) fn of(root: &'r Path) -> Option<Self> {
        let in_git = git::dot_git(root).is_ok_and(|dot_git| dot_git != DotGit::Absent);
        in_git.then(|| Self {
            root,
            uncommitted: None,
            walked: BTreeSet::new(),
            changes: BTreeMap::new(),
            by_commit: BTreeMap::new(),
        })
    }

    /// For each of `gone`, paths under the code root with nothing there,
    /// the path git renamed it to that the tree holds now, when git says
    /// exactly one and that path is still the file the rename made.
    pub(super) fn renamed(&mut self, gone: &BTreeSet<&str>) -> BTreeMap<String, Renamed> {
        let mut found = BTreeMap::new();
        let mut on_way: Vec<(String, Option<Reached>)> =
            gone.iter().map(|&path| (path.to_owned(), None)).collect();
        for hops in 0..=MAX_RENAME_HOPS {
            let last = hops == MAX_RENAME_HOPS;
            let at: Vec<String> = on_way
                .iter()
                .map(|(start, reached)| position(start, reached.as_ref()))
                .filter(|path| !last || self.root.join(path).exists())
                .collect();
            self.look_up(&at);
            let mut next = Vec::new();
            for (start, reached) in on_way {
                let reached = match reached {
                    Some(r) if !self.made_by(&r.renamed.to, r.commit.as_deref()) => continue,
                    Some(r) if self.root.join(&r.renamed.to).exists() => {
                        found.insert(start, r.renamed);
                        continue;
                    }
                    reached => reached,
                };
                if last {
                    continue;
                }
                let Some(hop) = self.hop(&position(&start, reached.as_ref())) else {
                    continue;
                };
                next.push((start, Some(extended(reached, hop))));
            }
            if next.is_empty() {
                break;
            }
            on_way = next;
        }
        found
    }

    /// For each of `files`, anchored paths under the code root, where it
    /// last held what the tree no longer does: the work tree, when it
    /// changed the file since `HEAD`, else before the last commit that
    /// changed it (for a file gone from `HEAD`, the one that deleted it).
    /// A path a walk asked about already is not walked again. A file git
    /// gives nothing of is left out.
    pub(super) fn judged(&mut self, files: &BTreeSet<&str>) -> BTreeMap<String, Judged> {
        let paths: Vec<&str> = files.iter().copied().collect();
        let Ok(changed) = git::changed_since(self.root, "HEAD", &paths) else {
            return BTreeMap::new();
        };
        let committed: Vec<String> = paths
            .into_iter()
            .filter(|path| !changed.contains(*path))
            .map(str::to_owned)
            .collect();
        self.look_up(&committed);
        let uncommitted = changed.into_iter().map(|path| (path, Judged::Uncommitted));
        let before = committed.into_iter().filter_map(|path| {
            // clone: the answer outlives what the history keeps.
            let last = self.changes.get(&path)?.last.clone();
            Some((path, Judged::Before(last)))
        });
        uncommitted.chain(before).collect()
    }

    /// The commits after `since`, newest first, that changed how many
    /// times `path` spells `name`; none when git fails.
    pub(super) fn spelling_changes(&self, since: &str, path: &str, name: &str) -> Vec<String> {
        git::renames::spelling_changes(self.root, since, path, name).unwrap_or_default()
    }

    /// The paths each of `commits` changed; none when git fails.
    pub(super) fn changed_by(
        &self,
        commits: &BTreeSet<&str>,
    ) -> BTreeMap<String, BTreeSet<String>> {
        changed_by(self.root, commits).unwrap_or_default()
    }

    /// Ask, in one walk, what the history says of each of `paths` no walk
    /// asked about yet.
    fn look_up(&mut self, paths: &[String]) {
        let paths: Vec<&str> = paths
            .iter()
            .map(String::as_str)
            .filter(|path| !self.walked.contains(*path))
            .collect();
        if paths.is_empty() {
            return;
        }
        #[cfg(test)]
        WALKS.with(|walks| walks.set(walks.get() + 1));
        self.walked
            .extend(paths.iter().map(|&path| path.to_owned()));
        if let Ok(changes) = last_changes(self.root, &paths) {
            self.changes.extend(changes);
        }
    }

    /// Whether the file at `path` is the one `commit` made: its newest
    /// addition is that commit. A rename not committed needs no check.
    fn made_by(&self, path: &str, commit: Option<&str>) -> bool {
        let Some(commit) = commit else {
            return true;
        };
        let added = self.changes.get(path).and_then(|c| c.added.as_deref());
        added == Some(commit)
    }

    /// The one path `path` was renamed to, not committed or in the commit
    /// that last changed, so deleted, it; `None` when git names none, a
    /// deletion, or several paths (a split shows as a copy beside the
    /// rename).
    fn hop(&mut self, path: &str) -> Option<Hop> {
        let uncommitted = self
            .uncommitted
            .get_or_insert_with(|| moved(self.root, Changes::Uncommitted).unwrap_or_default());
        if let Some(hop) = only_rename(uncommitted, path) {
            let (to, similarity) = hop?;
            return Some(Hop {
                to,
                similarity,
                commit: None,
            });
        }
        let commit = self.changes.get(path)?.last.as_str();
        if !self.by_commit.contains_key(commit) {
            let read: Result<Vec<Moved>> = moved(self.root, Changes::Commit(commit));
            let read = read.unwrap_or_default();
            self.by_commit.insert(commit.to_owned(), read);
        }
        let (to, similarity) = only_rename(self.by_commit.get(commit)?, path).flatten()?;
        Some(Hop {
            to,
            similarity,
            commit: Some(commit.to_owned()),
        })
    }
}

/// The path a gone path `start` has reached: where its renames so far
/// took it, or itself before the first.
fn position(start: &str, reached: Option<&Reached>) -> String {
    // clone: the next hop reads the path the last one reached.
    reached.map_or_else(|| start.to_owned(), |r| r.renamed.to.clone())
}

/// `reached`, or a gone path's start, carried one `hop` further.
fn extended(reached: Option<Reached>, hop: Hop) -> Reached {
    let exact = reached.as_ref().is_none_or(|r| r.renamed.exact) && hop.similarity == EXACT;
    let before = match reached {
        Some(reached) => reached.renamed.before,
        None => hop
            .commit
            .as_ref()
            .map_or_else(|| "HEAD".to_owned(), |commit| format!("{commit}^")),
    };
    Reached {
        renamed: Renamed {
            to: hop.to,
            exact,
            before,
        },
        commit: hop.commit,
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
