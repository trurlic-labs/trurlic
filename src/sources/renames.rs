//! What the calling branch's history says of the files of stale anchors,
//! read once a call in two git processes: the walk of the newest commits
//! and what the work tree and the index changed against `HEAD`
//! ([`History::read`]). It is read only for `check` and the
//! `repair_anchors` op, only when a stale anchor may be repaired, and only
//! when a `.git` is at or above the code root.
//!
//! A gone file is followed by its blob, the bytes git names by their hash:
//! the commit that deleted its path, or the work tree, moved it where it
//! was the one path to lose that blob and exactly one other path gained
//! it, so git's similarity scoring, which pairs files by a shared header,
//! plays no part. From there each later change of the path it reached is
//! judged in order: one that moves the blob on, its path deleted or
//! written anew, follows it; an edit in place keeps it there with the
//! edited blob; a deletion, a blob two paths gained or two lost, or a
//! change whose blob before is not the one followed (a side branch's)
//! leaves no answer, whatever fills the path now; and what is not
//! committed is judged last. A file moved without git and not yet added is
//! no move git can see. A failure of git leaves the anchors concerned
//! stale without a repair, as they are.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::git::renames::{Change, Commit, uncommitted, walk};
use crate::git::{self, DotGit};

/// The history read: the walk's commits, newest first, the changes of each
/// path by commit, newest first, and the changes not committed.
pub(super) struct History {
    commits: Vec<Commit>,
    by_path: BTreeMap<String, Vec<(usize, usize)>>,
    uncommitted: Vec<Change>,
}

/// Where git's history shows a gone file now: its path, the blob that left
/// the gone path, and the blob after each edit of it in place since, oldest
/// first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Followed {
    pub(super) to: String,
    pub(super) moved: String,
    pub(super) edits: Vec<String>,
}

/// Where a change leaves the file followed.
enum Step {
    /// Its blob moved to this path.
    Moved(String),
    /// It was edited in place into this blob.
    Edited(String),
    /// It is not known to be anywhere.
    Left,
}

impl History {
    /// The history of the repository holding `root`, the code root; `None`
    /// when no `.git` is at or above it, so git is asked nothing, or git
    /// fails.
    pub(super) fn read(root: &Path) -> Option<Self> {
        if git::dot_git(root).ok()? == DotGit::Absent {
            return None;
        }
        let commits = walk(root).ok()?;
        let mut by_path: BTreeMap<String, Vec<(usize, usize)>> = BTreeMap::new();
        for (at, commit) in commits.iter().enumerate() {
            for (place, change) in commit.changes.iter().enumerate() {
                // clone: the index keys each path the commits list.
                let path = change.path.clone();
                by_path.entry(path).or_default().push((at, place));
            }
        }
        Some(Self {
            commits,
            by_path,
            uncommitted: uncommitted(root).unwrap_or_default(),
        })
    }

    /// The changes of `path` in the walk, newest first, each with its
    /// commit's place in the walk.
    pub(super) fn changes<'h>(&'h self, path: &str) -> impl Iterator<Item = (usize, &'h Change)> {
        let places = self.by_path.get(path).map_or(&[][..], Vec::as_slice);
        places.iter().filter_map(|&(at, place)| {
            let change = self.commits.get(at)?.changes.get(place)?;
            Some((at, change))
        })
    }

    /// What the work tree and the index changed at `path` against `HEAD`.
    pub(super) fn uncommitted(&self, path: &str) -> Option<&Change> {
        self.uncommitted.iter().find(|change| change.path == path)
    }

    /// For each of `gone`, paths under the code root with nothing there,
    /// the path its file is at now, when its blob's moves show one.
    pub(super) fn renamed(&self, gone: &BTreeSet<&str>) -> BTreeMap<String, Followed> {
        gone.iter()
            .filter_map(|&path| Some((path.to_owned(), self.followed(path)?)))
            .collect()
    }

    /// Where the file at `gone` is now: from the work tree's deletion of
    /// it, else from the commit that last changed, so deleted, it.
    fn followed(&self, gone: &str) -> Option<Followed> {
        if let Some(change) = self.uncommitted(gone) {
            // clone: the blob followed is also the answer's.
            let moved = change.before.clone()?;
            let to = self.in_work_tree(gone, &moved)?;
            let edits = Vec::new();
            return Some(Followed { to, moved, edits });
        }
        let (at, change) = self.changes(gone).next()?;
        // clone: as above.
        let moved = change.before.clone()?;
        // clone: as above.
        let (to, edits) = self.kept(gone.to_owned(), moved.clone(), at + 1)?;
        Some(Followed { to, moved, edits })
    }

    /// Where the file that held `blob` at `path` after the commit before
    /// place `after` of the walk is now, and the blob after each edit on the
    /// way: each later change of its path judged, oldest first, then the
    /// work tree.
    fn kept(
        &self,
        mut path: String,
        mut blob: String,
        mut after: usize,
    ) -> Option<(String, Vec<String>)> {
        let mut edits = Vec::new();
        loop {
            let next = self.changes(&path).take_while(|&(at, _)| at < after).last();
            let Some((at, change)) = next else {
                break;
            };
            let changes = &self.commits.get(at)?.changes;
            match step(changes, change, &blob) {
                Step::Moved(to) => path = to,
                Step::Edited(to) => {
                    // clone: the blob followed is also the edit's.
                    edits.push(to.clone());
                    blob = to;
                }
                Step::Left => return None,
            }
            after = at;
        }
        Some((self.in_work_tree(&path, &blob)?, edits))
    }

    /// Where the work tree left the file that holds `blob` at `path` in
    /// `HEAD`: moved on, kept there, or not known to be anywhere.
    fn in_work_tree(&self, path: &str, blob: &str) -> Option<String> {
        let Some(change) = self.uncommitted(path) else {
            return Some(path.to_owned());
        };
        if change.before.as_deref() != Some(blob) {
            return None;
        }
        match step(&self.uncommitted, change, blob) {
            Step::Moved(to) => Some(to),
            // An edit the index does not hold, which git does not hash.
            Step::Edited(_) | Step::Left if change.status == b'M' => Some(path.to_owned()),
            Step::Edited(_) | Step::Left => None,
        }
    }
}

/// What `change`, one of `changes` made together, did to the file that held
/// `blob` at its path: moved it when it is the one path that lost the blob
/// and exactly one other path gained it, edited it in place, or left it
/// unknown.
fn step(changes: &[Change], change: &Change, blob: &str) -> Step {
    if change.before.as_deref() != Some(blob) {
        return Step::Left;
    }
    let holds = |side: &Option<String>| side.as_deref() == Some(blob);
    let mut gained = changes
        .iter()
        .filter(|other| other.path != change.path && holds(&other.after));
    let lost = changes
        .iter()
        .filter(|other| holds(&other.before) && !holds(&other.after))
        .count();
    match (gained.next(), gained.next(), lost) {
        // clone: the path followed outlives the history's borrow.
        (Some(to), None, 1) => return Step::Moved(to.path.clone()),
        (Some(_), _, _) => return Step::Left,
        (None, _, _) => {}
    }
    match (change.status, &change.after) {
        // clone: as above.
        (b'M', Some(after)) => Step::Edited(after.clone()),
        _ => Step::Left,
    }
}

#[cfg(test)]
mod tests;
