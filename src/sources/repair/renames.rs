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

    /// The commit `HEAD` names, the walk's newest: none on an unborn branch.
    pub(super) fn head(&self) -> Option<&str> {
        self.commits.first().map(|commit| commit.id.as_str())
    }

    /// What the work tree and the index changed at `path` against `HEAD`.
    pub(super) fn uncommitted(&self, path: &str) -> Option<&Change> {
        self.uncommitted.iter().find(|change| change.path == path)
    }

    /// The changes the commit at place `at` of the walk made, against its
    /// first parent; none past the walk.
    pub(super) fn made(&self, at: usize) -> &[Change] {
        self.commits
            .get(at)
            .map_or(&[][..], |commit| &commit.changes)
    }

    /// The commits the file at `path` now came through, each with the path
    /// it had there: each commit that changed it and, where one added it as
    /// the one path to gain a blob one other path lost, those of that path
    /// before.
    pub(super) fn lineage(&self, path: &str) -> BTreeMap<usize, String> {
        let mut lineage = BTreeMap::new();
        // clone: the path followed changes as the file came from others.
        let (mut path, mut older) = (path.to_owned(), None);
        loop {
            let mut added = None;
            let changes = self.changes(&path);
            for (at, change) in changes.filter(|&(at, _)| older.is_none_or(|than| at > than)) {
                // clone: each commit names the path it changed.
                lineage.insert(at, path.clone());
                if change.status == b'A' {
                    added = change.after.as_deref().map(|blob| (at, blob));
                    break;
                }
            }
            let Some((at, blob)) = added else {
                return lineage;
            };
            let lost = |other: &&Change| {
                other.before.as_deref() == Some(blob) && other.after.as_deref() != Some(blob)
            };
            let mut from = self.made(at).iter().filter(lost);
            let (Some(from), None) = (from.next(), from.next()) else {
                return lineage;
            };
            // clone: as above.
            (path, older) = (from.path.clone(), Some(at));
        }
    }

    /// For each of `gone`, paths under the code root with nothing there,
    /// the path its file is at now, when its blob's moves show one.
    pub(super) fn renamed(&self, gone: &BTreeSet<&str>) -> BTreeMap<String, String> {
        gone.iter()
            .filter_map(|&path| Some((path.to_owned(), self.followed(path)?)))
            .collect()
    }

    /// Where the file at `gone` is now: from the work tree's deletion of
    /// it, else from the commit that last changed, so deleted, it.
    fn followed(&self, gone: &str) -> Option<String> {
        if let Some(change) = self.uncommitted(gone) {
            return self.in_work_tree(gone, change.before.as_deref()?);
        }
        let (at, change) = self.changes(gone).next()?;
        // clone: the blob followed changes as the file moves.
        let (to, _) = self.kept(gone.to_owned(), change.before.clone()?, at + 1)?;
        Some(to)
    }

    /// Where the file that held `blob` at `path` after the commit before
    /// place `after` of the walk is now, and the blob after each edit on the
    /// way: each later change of its path judged, oldest first, then the
    /// work tree.
    pub(super) fn kept(
        &self,
        mut path: String,
        mut blob: String,
        after: usize,
    ) -> Option<(String, Vec<String>)> {
        let mut edits = Vec::new();
        let mut newer = self.newer(&path, after);
        while let Some((at, place)) = newer.pop() {
            let changes = &self.commits.get(at)?.changes;
            let change = changes.get(place)?;
            match step(changes, change, &blob) {
                Step::Moved(to) => {
                    newer = self.newer(&to, at);
                    path = to;
                }
                Step::Edited(to) => {
                    // clone: the blob followed is also the edit's.
                    edits.push(to.clone());
                    blob = to;
                }
                Step::Left => return None,
            }
        }
        Some((self.in_work_tree(&path, &blob)?, edits))
    }

    /// The places of `path`'s changes newer than the commit at place
    /// `after` of the walk, newest first.
    fn newer(&self, path: &str, after: usize) -> Vec<(usize, usize)> {
        let places = self.by_path.get(path).map_or(&[][..], Vec::as_slice);
        places
            .iter()
            .copied()
            .take_while(|&(at, _)| at < after)
            .collect()
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
    let lost = || {
        let lost = changes
            .iter()
            .filter(|other| holds(&other.before) && !holds(&other.after));
        lost.count()
    };
    match (gained.next(), gained.next()) {
        // clone: the path followed outlives the history's borrow.
        (Some(to), None) if lost() == 1 => return Step::Moved(to.path.clone()),
        (Some(_), _) => return Step::Left,
        (None, _) => {}
    }
    match (change.status, &change.after) {
        // clone: as above.
        (b'M', Some(after)) => Step::Edited(after.clone()),
        _ => Step::Left,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;

    use tempfile::TempDir;

    use super::*;

    /// `git <args>` in `dir`, isolated from the user's and the system's
    /// configuration, with a fixed identity; it must succeed.
    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .current_dir(dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .args(["-c", "user.name=t", "-c", "user.email=t@t.invalid"])
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    /// Lines enough for git to find a renamed file similar to its source.
    const BODY: &str = "fn a() {}\nfn b() {}\nfn c() {}\nfn d() {}\nfn e() {}\nfn f() {}\n";

    /// A repository at `dir` on `main` whose first commit holds `files`, each
    /// its own text.
    fn repository(dir: &Path, files: &[&str]) {
        for file in files {
            let path = dir.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, format!("// {file}\n{BODY}")).unwrap();
        }
        git(dir, &["init", "--quiet", "--initial-branch=main"]);
        git(dir, &["add", "--all"]);
        git(dir, &["commit", "--quiet", "-m", "first"]);
    }

    /// Every change of the work tree committed as one commit.
    fn commit(dir: &Path, message: &str) {
        git(dir, &["add", "--all"]);
        git(dir, &["commit", "--quiet", "-m", message]);
    }

    /// Where the history of the repository at `dir` shows each of `gone` now;
    /// nothing with no `.git` there.
    fn renamed(dir: &Path, gone: &[&str]) -> BTreeMap<String, String> {
        let gone: BTreeSet<&str> = gone.iter().copied().collect();
        let Some(history) = History::read(dir) else {
            return BTreeMap::new();
        };
        history.renamed(&gone)
    }

    fn to(from: &str, to: &str) -> (String, String) {
        (from.to_owned(), to.to_owned())
    }

    /// A file moved unchanged is followed by its blob: in a commit, through a
    /// second move, and by a `git mv` not committed. One moved and edited in
    /// the same commit, one copied beside its move, so two paths gained its
    /// blob, two of one content that one file took the place of, and one
    /// deleted get no answer, nor one moved without git.
    #[test]
    fn a_gone_path_gets_the_path_its_blob_moved_to() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        repository(
            dir,
            &["a.rs", "b.rs", "s.rs", "d.rs", "u.rs", "e.rs", "w.rs"],
        );
        for twin in ["t1.rs", "t2.rs"] {
            fs::write(dir.join(twin), "fn twin() {}\n").unwrap();
        }
        commit(dir, "twins");
        git(dir, &["mv", "a.rs", "a1.rs"]);
        git(dir, &["mv", "e.rs", "e1.rs"]);
        fs::write(dir.join("e1.rs"), format!("// e.rs\n{BODY}fn g() {{}}\n")).unwrap();
        git(dir, &["mv", "b.rs", "b1.rs"]);
        commit(dir, "renames");
        git(dir, &["rm", "--quiet", "d.rs"]);
        commit(dir, "deletion");
        for twin in ["t1.rs", "t2.rs"] {
            git(dir, &["rm", "--quiet", twin]);
        }
        fs::write(dir.join("t.rs"), "fn twin() {}\n").unwrap();
        commit(dir, "twins gone");
        fs::create_dir(dir.join("src")).unwrap();
        git(dir, &["mv", "b1.rs", "src/b2.rs"]);
        fs::copy(dir.join("s.rs"), dir.join("s1.rs")).unwrap();
        fs::rename(dir.join("s.rs"), dir.join("s2.rs")).unwrap();
        commit(dir, "more");
        git(dir, &["mv", "u.rs", "u1.rs"]);
        fs::rename(dir.join("w.rs"), dir.join("w1.rs")).unwrap();

        let gone = [
            "a.rs", "b.rs", "s.rs", "d.rs", "u.rs", "e.rs", "w.rs", "t1.rs", "t2.rs", "x.rs",
        ];
        let found = renamed(dir, &gone);

        assert_eq!(
            found,
            BTreeMap::from([
                to("a.rs", "a1.rs"),
                to("b.rs", "src/b2.rs"),
                to("u.rs", "u1.rs")
            ])
        );
    }

    /// A moved file is judged by every later change of the path it reached,
    /// oldest first: edits in place keep it there, and a later move takes it
    /// on; a deletion leaves no answer, though a later commit writes the path
    /// again; and a move away while a new file takes the path, in one commit
    /// or not committed, follows the file, not the path.
    #[test]
    fn a_moved_file_is_followed_through_every_later_change_of_its_path() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        repository(dir, &["edited.rs", "deleted.rs", "refilled.rs", "away.rs"]);
        for file in ["edited", "deleted", "refilled", "away"] {
            git(dir, &["mv", &format!("{file}.rs"), &format!("{file}1.rs")]);
        }
        commit(dir, "moves");
        fs::write(dir.join("edited1.rs"), format!("{BODY}fn g() {{}}\n")).unwrap();
        fs::write(dir.join("deleted1.rs"), format!("{BODY}fn h() {{}}\n")).unwrap();
        commit(dir, "edits");
        git(dir, &["mv", "edited1.rs", "edited2.rs"]);
        git(dir, &["rm", "--quiet", "deleted1.rs"]);
        git(dir, &["mv", "refilled1.rs", "refilled2.rs"]);
        fs::write(dir.join("refilled1.rs"), "fn unrelated() {}\n").unwrap();
        commit(dir, "more");
        fs::write(dir.join("deleted1.rs"), "fn unrelated() {}\n").unwrap();
        commit(dir, "written again");
        git(dir, &["mv", "away1.rs", "away2.rs"]);
        fs::write(dir.join("away1.rs"), "fn unrelated() {}\n").unwrap();
        git(dir, &["add", "--all"]);

        let found = renamed(dir, &["edited.rs", "deleted.rs", "refilled.rs", "away.rs"]);

        assert_eq!(
            found,
            BTreeMap::from([
                to("away.rs", "away2.rs"),
                to("edited.rs", "edited2.rs"),
                to("refilled.rs", "refilled2.rs"),
            ])
        );
    }

    /// A file is followed from the commit its blob reached a path in: through
    /// each later edit, whose blob it names, oldest first, to where it is now.
    #[test]
    fn a_kept_file_names_each_edit_since() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        repository(dir, &["a.rs"]);
        git(dir, &["mv", "a.rs", "b.rs"]);
        commit(dir, "move");
        for edit in ["fn x() {}\n", "fn y() {}\n"] {
            fs::write(dir.join("b.rs"), format!("{BODY}{edit}")).unwrap();
            commit(dir, "edit");
        }
        let blob = |revision: &str| {
            let output = Command::new("git")
                .current_dir(dir)
                .args(["rev-parse", &format!("{revision}:b.rs")])
                .output()
                .unwrap();
            String::from_utf8(output.stdout).unwrap().trim().to_owned()
        };

        let history = History::read(dir).unwrap();
        let kept = history.kept("b.rs".to_owned(), blob("HEAD~2"), 2);

        let edits = vec![blob("HEAD~1"), blob("HEAD")];
        assert_eq!(kept, Some(("b.rs".to_owned(), edits)));
    }

    /// The changes of a path come newest first, each with its commit's place
    /// in the walk, and what the work tree changed is apart: a deleted file's
    /// newest change is its deletion.
    #[test]
    fn a_paths_changes_come_newest_first_and_the_work_tree_apart() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        repository(dir, &["kept.rs", "edited.rs", "gone.rs", "dirty.rs"]);
        fs::write(dir.join("edited.rs"), "fn changed() {}\n").unwrap();
        git(dir, &["rm", "--quiet", "gone.rs"]);
        commit(dir, "second");
        fs::write(dir.join("dirty.rs"), "").unwrap();

        let history = History::read(dir).unwrap();

        let changes = |path: &str| -> Vec<(usize, u8)> {
            let changes = history.changes(path);
            changes.map(|(at, change)| (at, change.status)).collect()
        };
        assert_eq!(changes("edited.rs"), [(0, b'M'), (1, b'A')]);
        assert_eq!(changes("gone.rs"), [(0, b'D'), (1, b'A')]);
        assert_eq!(changes("kept.rs"), [(1, b'A')]);
        assert_eq!(changes("never.rs"), []);
        assert_eq!(
            history.uncommitted("dirty.rs").map(|c| c.status),
            Some(b'M')
        );
        assert!(history.uncommitted("edited.rs").is_none());
    }

    /// What a user's configuration may set in the repository changes no
    /// answer: a signature check, the root commit's paths, rename detection,
    /// abbreviated ids and color are pinned or overridden.
    #[test]
    fn a_users_configuration_changes_no_answer() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        repository(dir, &["a.rs", "gone.rs"]);
        git(dir, &["mv", "a.rs", "a1.rs"]);
        git(dir, &["rm", "--quiet", "gone.rs"]);
        commit(dir, "renames");
        let changes = |dir: &Path| {
            let history = History::read(dir).unwrap();
            let changes = history.changes("gone.rs");
            changes
                .map(|(at, change)| (at, change.clone()))
                .collect::<Vec<_>>()
        };
        let clean = (renamed(dir, &["a.rs"]), changes(dir));
        for setting in [
            "log.showSignature=true",
            "log.showRoot=false",
            "diff.renames=copies",
            "core.abbrev=7",
            "color.ui=always",
            "color.diff=always",
        ] {
            let (key, value) = setting.split_once('=').unwrap();
            git(dir, &["config", key, value]);
        }

        let hostile = (renamed(dir, &["a.rs"]), changes(dir));

        assert_eq!(clean.0.len(), 1, "{clean:?}");
        assert_eq!(clean.1.len(), 2, "{clean:?}");
        assert_eq!(hostile, clean);
    }

    /// The code root may be a subdirectory of its work tree: paths are asked
    /// and answered relative to it, and a move out of it is no answer.
    #[test]
    fn paths_are_relative_to_the_code_root() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        repository(dir, &["sub/a.rs", "sub/b.rs"]);
        fs::create_dir(dir.join("sub/src")).unwrap();
        git(dir, &["mv", "sub/a.rs", "sub/src/a.rs"]);
        git(dir, &["mv", "sub/b.rs", "b.rs"]);
        commit(dir, "moves");

        let found = renamed(&dir.join("sub"), &["a.rs", "b.rs"]);

        assert_eq!(found, BTreeMap::from([to("a.rs", "src/a.rs")]));
    }

    /// A tree with no git has no history to ask, and an unborn branch none
    /// that answers.
    #[test]
    fn no_history_gives_no_answer() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        fs::write(dir.join("a1.rs"), BODY).unwrap();
        assert!(History::read(dir).is_none());
        git(dir, &["init", "--quiet", "--initial-branch=main"]);
        assert!(History::read(dir).is_none());
    }
}
