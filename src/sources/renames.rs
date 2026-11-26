//! What the calling branch's history says of the files of stale anchors:
//! where a gone file went, and the commits that changed an anchored file.
//!
//! The history is read only for `check` and the `repair_anchors` op, and
//! only when a `.git` is at or above the code root ([`History::of`]). What
//! is not committed comes first: a rename between `HEAD` and the work tree
//! or the index, as `git mv` leaves it (a file moved without git and not
//! yet added is no rename git can see). Else the calling branch's history:
//! the most recent commit that changed the gone path, which deleted it,
//! and what that commit renamed it to. The path a rename reached is then
//! judged by the next commit that changed it: none, and the path holds the
//! rename's file still; a rename of it, followed from there, at most
//! [`MAX_RENAME_HOPS`] times; a deletion, a copy or several records, and
//! the rename's file left the path, which something else may fill now: no
//! answer. One walk of the history serves every path of a hop, and each
//! commit is diffed once. A path renamed to several, or copied beside the
//! rename, was split: no answer. The answer is exact when every rename on
//! the way has git's similarity 100, and names the revision before the
//! first rename and the commit of the last. A failure of git leaves the
//! paths it concerns without an answer, which leaves their anchors stale
//! without a repair, as they are.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::Result;
use crate::git::renames::{Change, Changes, Moved, changes, moved};
use crate::git::{self, DotGit};
use crate::lint::anchors::Renamed;

/// The renames followed from one gone path before the answer is given up.
pub(crate) const MAX_RENAME_HOPS: usize = 8;

/// git's similarity of a rename that kept the content whole.
const EXACT: u8 = 100;

/// What git said so far of the repository holding the code root: the
/// uncommitted renames, once asked, the commits that changed each path a
/// walk asked about, newest first, and the renames of each commit diffed.
pub(super) struct History<'r> {
    root: &'r Path,
    uncommitted: Option<Vec<Moved>>,
    walked: BTreeSet<String>,
    changes: BTreeMap<String, Vec<Change>>,
    by_commit: BTreeMap<String, Vec<Moved>>,
}

/// One rename git reports of a path: where it went, git's similarity, and
/// the commit that made it, `None` for one not committed.
struct Hop {
    to: String,
    similarity: u8,
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
    /// the path git renamed it to that still holds the rename's file, when
    /// git says exactly one.
    pub(super) fn renamed(&mut self, gone: &BTreeSet<&str>) -> BTreeMap<String, Renamed> {
        let mut found = BTreeMap::new();
        let mut on_way: Vec<(String, Option<Renamed>)> =
            gone.iter().map(|&path| (path.to_owned(), None)).collect();
        for hops in 0..=MAX_RENAME_HOPS {
            let last = hops == MAX_RENAME_HOPS;
            let at: Vec<String> = on_way
                .iter()
                // clone: the walk is asked the paths apart from the chains.
                .map(|(start, reached)| reached.as_ref().map_or(start, |r| &r.to).clone())
                .filter(|path| !last || self.root.join(path).exists())
                .collect();
            self.look_up(&at);
            let mut next = Vec::new();
            for (start, reached) in on_way {
                let hop = match &reached {
                    None => self.first_hop(&start),
                    Some(renamed) => match self.after(renamed) {
                        After::Holds => {
                            // clone: the answer outlives the chain it ends.
                            found.insert(start, renamed.clone());
                            continue;
                        }
                        After::Renamed(hop) => Some(hop),
                        After::Left => None,
                    },
                };
                if let Some(hop) = hop.filter(|_| !last) {
                    next.push((start, Some(extended(reached, hop))));
                }
            }
            if next.is_empty() {
                break;
            }
            on_way = next;
        }
        found
    }

    /// For each of `files`, the commits of the calling branch that changed
    /// it, newest first; a file the history never held is left out.
    pub(super) fn commits(&mut self, files: &BTreeSet<&str>) -> BTreeMap<String, Vec<Change>> {
        let paths: Vec<String> = files.iter().map(|&path| path.to_owned()).collect();
        self.look_up(&paths);
        paths
            .into_iter()
            // clone: the answer outlives what the history keeps.
            .filter_map(|path| Some((self.changes.get(&path)?.clone(), path)))
            .map(|(commits, path)| (path, commits))
            .collect()
    }

    /// Which of `files` the work tree changed since `HEAD`, untracked ones
    /// included; none when git fails.
    pub(super) fn uncommitted(&self, files: &BTreeSet<&str>) -> BTreeSet<String> {
        let paths: Vec<&str> = files.iter().copied().collect();
        git::changed_since(self.root, "HEAD", &paths).unwrap_or_default()
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
        if let Ok(found) = changes(self.root, &paths) {
            self.changes.extend(found);
        }
    }

    /// The one path the gone `path` was renamed to, not committed or in the
    /// commit that last changed, so deleted, it.
    fn first_hop(&mut self, path: &str) -> Option<Hop> {
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
        // clone: the commit outlives the borrow of the history.
        let commit = self.changes.get(path)?.first()?.commit.clone();
        self.renamed_in(path, commit)
    }

    /// What the next commit that changed `renamed.to` after the rename
    /// that reached it did to it.
    fn after(&mut self, renamed: &Renamed) -> After {
        let Some(made) = &renamed.made else {
            return After::Holds;
        };
        let commits = self.changes.get(&renamed.to).map_or(&[][..], Vec::as_slice);
        let Some(at) = commits.iter().position(|change| change.commit == *made) else {
            return After::Left;
        };
        let Some(next) = at.checked_sub(1).and_then(|newer| commits.get(newer)) else {
            return match self.root.join(&renamed.to).exists() {
                true => After::Holds,
                false => After::Left,
            };
        };
        // clone: the commit outlives the borrow of the history.
        let next = next.commit.clone();
        match self
            .diffed(&next)
            .iter()
            .any(|change| change.from == renamed.to)
        {
            false => After::Holds,
            true => self
                .renamed_in(&renamed.to, next)
                .map_or(After::Left, After::Renamed),
        }
    }

    /// The one path `commit` renamed `path` to; `None` for a deletion, a
    /// copy, or several records.
    fn renamed_in(&mut self, path: &str, commit: String) -> Option<Hop> {
        let (to, similarity) = only_rename(self.diffed(&commit), path)??;
        Some(Hop {
            to,
            similarity,
            commit: Some(commit),
        })
    }

    /// The renames, copies and deletions of `commit`, diffed once.
    fn diffed(&mut self, commit: &str) -> &[Moved] {
        if !self.by_commit.contains_key(commit) {
            let read: Result<Vec<Moved>> = moved(self.root, Changes::Commit(commit));
            self.by_commit
                .insert(commit.to_owned(), read.unwrap_or_default());
        }
        self.by_commit.get(commit).map_or(&[][..], Vec::as_slice)
    }
}

/// What the next commit that changed the path a rename reached did to it.
enum After {
    /// Nothing, or edited it: the path holds the rename's file.
    Holds,
    /// Renamed it on.
    Renamed(Hop),
    /// Deleted, copied or split it, or the history cannot say: the rename's
    /// file is not known to be there.
    Left,
}

/// `reached`, or a gone path's start, carried one `hop` further.
fn extended(reached: Option<Renamed>, hop: Hop) -> Renamed {
    let exact = reached.as_ref().is_none_or(|r| r.exact) && hop.similarity == EXACT;
    let before = match reached {
        Some(reached) => reached.before,
        None => hop
            .commit
            .as_ref()
            .map_or_else(|| "HEAD".to_owned(), |commit| format!("{commit}^")),
    };
    Renamed {
        to: hop.to,
        exact,
        before,
        made: hop.commit,
    }
}

/// What `changes` made of `path`: `None` when nothing, `Some(None)` when a
/// deletion, a copy or several records, `Some(Some((to, similarity)))` for
/// one rename. A split shows as two records, a copy beside the rename.
fn only_rename(changes: &[Moved], path: &str) -> Option<Option<(String, u8)>> {
    let mut from = changes.iter().filter(|change| change.from == path);
    let first = from.next()?;
    let one = from.next().is_none() && !first.copy;
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
