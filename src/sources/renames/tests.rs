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
