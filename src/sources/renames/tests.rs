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

/// A repository at `dir` on `main` whose first commit holds `files`.
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

fn asked<'a>(paths: &[&'a str]) -> BTreeSet<&'a str> {
    paths.iter().copied().collect()
}

/// What the history of the repository at `dir` says each of `gone` was
/// renamed to; nothing with no `.git` there.
fn renamed(dir: &Path, gone: &BTreeSet<&str>) -> BTreeMap<String, Renamed> {
    History::of(dir).map_or_else(BTreeMap::new, |mut history| history.renamed(gone))
}

/// A rename committed, one followed through a second rename, one made with
/// `git mv` and not committed, one that changed the content on the way,
/// and a file a commit split in two or deleted: each gone path gets the
/// path the tree holds, exact while every rename kept its content whole,
/// or no answer.
#[test]
fn a_gone_path_gets_the_path_git_renamed_it_to() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    repository(dir, &["a.rs", "b.rs", "s.rs", "d.rs", "u.rs", "e.rs"]);
    git(dir, &["mv", "a.rs", "a1.rs"]);
    git(dir, &["mv", "e.rs", "e1.rs"]);
    fs::write(dir.join("e1.rs"), format!("// e.rs\n{BODY}fn g() {{}}\n")).unwrap();
    git(dir, &["add", "e1.rs"]);
    git(dir, &["mv", "b.rs", "b1.rs"]);
    git(dir, &["rm", "--quiet", "d.rs"]);
    git(dir, &["commit", "--quiet", "-m", "renames"]);
    fs::create_dir(dir.join("src")).unwrap();
    git(dir, &["mv", "b1.rs", "src/b2.rs"]);
    fs::copy(dir.join("s.rs"), dir.join("s1.rs")).unwrap();
    fs::rename(dir.join("s.rs"), dir.join("s2.rs")).unwrap();
    git(dir, &["add", "--all"]);
    git(dir, &["commit", "--quiet", "-m", "more"]);
    git(dir, &["mv", "u.rs", "u1.rs"]);

    let found = renamed(
        dir,
        &asked(&["a.rs", "b.rs", "s.rs", "d.rs", "u.rs", "e.rs", "never.rs"]),
    );

    // The commit `renames` made the first rename of each committed path,
    // and `more` the second of `b.rs`.
    let (renames, more) = (head(dir, "HEAD~1"), head(dir, "HEAD"));
    let mut b = to("b.rs", "src/b2.rs", true, Some(&more));
    b.1.before = format!("{renames}^");
    assert_eq!(
        found,
        BTreeMap::from([
            to("a.rs", "a1.rs", true, Some(&renames)),
            b,
            to("e.rs", "e1.rs", false, Some(&renames)),
            to("u.rs", "u1.rs", true, None),
        ])
    );
}

/// The commit `revision` names in the repository at `dir`.
fn head(dir: &Path, revision: &str) -> String {
    let output = Command::new("git")
        .current_dir(dir)
        .args(["rev-parse", revision])
        .output()
        .unwrap();
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn to(from: &str, to: &str, exact: bool, made: Option<&str>) -> (String, Renamed) {
    let renamed = Renamed {
        to: to.to_owned(),
        exact,
        before: made.map_or_else(|| "HEAD".to_owned(), |commit| format!("{commit}^")),
        made: made.map(str::to_owned),
    };
    (from.to_owned(), renamed)
}

/// The commits that changed each anchored file, newest first, each with
/// whether it added or deleted the file, from one walk that a rename's
/// walk shares, and the files the work tree changed
/// since `HEAD`; a file the history never held has no commits.
#[test]
fn an_anchored_files_commits_come_newest_first_from_the_walk_its_rename_shares() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    repository(dir, &["kept.rs", "edited.rs", "gone.rs", "dirty.rs"]);
    fs::write(dir.join("edited.rs"), "fn changed() {}\n").unwrap();
    git(dir, &["rm", "--quiet", "gone.rs"]);
    git(dir, &["commit", "--quiet", "-a", "-m", "second"]);
    fs::write(dir.join("dirty.rs"), "").unwrap();
    let (first, second) = (head(dir, "HEAD~1"), head(dir, "HEAD"));
    let mut history = History::of(dir).unwrap();
    assert!(history.renamed(&asked(&["gone.rs"])).is_empty());
    WALKS.with(|walks| walks.set(0));

    let files = asked(&["kept.rs", "edited.rs", "gone.rs", "dirty.rs", "never.rs"]);
    let commits = history.commits(&files);

    assert_eq!(
        WALKS.with(std::cell::Cell::get),
        1,
        "gone.rs is walked once"
    );
    let change = |commit: &String, replaced| Change {
        commit: commit.clone(),
        replaced,
    };
    let at = |path: &str, changes: Vec<Change>| (path.to_owned(), changes);
    assert_eq!(
        commits,
        BTreeMap::from([
            at("dirty.rs", vec![change(&first, true)]),
            at(
                "edited.rs",
                vec![change(&second, false), change(&first, true)]
            ),
            at("gone.rs", vec![change(&second, true), change(&first, true)]),
            at("kept.rs", vec![change(&first, true)]),
        ])
    );
    assert_eq!(
        history.uncommitted(&files),
        BTreeSet::from(["dirty.rs".to_owned()])
    );
}

/// A rename's new path is judged by the next commit that changed it: one
/// that deleted it, or replaced it while it moved elsewhere (a copy record
/// from it), leaves no answer whatever fills the path now; one that renamed
/// it on is followed; one that edited it keeps the answer.
#[test]
fn a_rename_to_a_path_an_unrelated_file_took_later_is_no_answer() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    repository(dir, &["old.rs", "a.rs"]);
    git(dir, &["mv", "old.rs", "new.rs"]);
    git(dir, &["mv", "a.rs", "b.rs"]);
    git(dir, &["commit", "--quiet", "-m", "renames"]);
    git(dir, &["rm", "--quiet", "new.rs"]);
    git(dir, &["mv", "b.rs", "c.rs"]);
    git(dir, &["commit", "--quiet", "-m", "away"]);
    fs::write(
        dir.join("new.rs"),
        "fn unrelated() {}
",
    )
    .unwrap();
    fs::write(
        dir.join("b.rs"),
        "fn kept() {}
",
    )
    .unwrap();
    git(dir, &["add", "--all"]);
    git(dir, &["commit", "--quiet", "-m", "taken"]);

    let found = renamed(dir, &asked(&["old.rs", "a.rs"]));

    let away = head(dir, "HEAD~1");
    let mut a = to("a.rs", "c.rs", true, Some(&away));
    a.1.before = format!("{}^", head(dir, "HEAD~2"));
    assert_eq!(found, BTreeMap::from([a]));
}

/// A path vacated and filled again in one commit, its file moving on while
/// a new one takes the path, gives no answer; a path edited after the
/// rename keeps it.
#[test]
fn a_rename_to_a_path_refilled_in_one_commit_is_no_answer_and_an_edit_keeps_one() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    repository(dir, &["a.rs", "e.rs"]);
    git(dir, &["mv", "a.rs", "b.rs"]);
    fs::write(dir.join("e.rs"), format!("// e.rs\n{BODY}fn g() {{}}\n")).unwrap();
    git(dir, &["mv", "e.rs", "v.rs"]);
    git(dir, &["commit", "--quiet", "-a", "-m", "renames"]);
    let renames = head(dir, "HEAD");
    git(dir, &["mv", "b.rs", "c.rs"]);
    fs::write(dir.join("b.rs"), "fn kept() -> String { String::new() }\n").unwrap();
    fs::write(dir.join("v.rs"), format!("// e.rs\n{BODY}fn g() {{ 1 }}\n")).unwrap();
    git(dir, &["add", "--all"]);
    git(dir, &["commit", "--quiet", "-m", "refill"]);

    let found = renamed(dir, &asked(&["a.rs", "e.rs"]));

    assert_eq!(
        found,
        BTreeMap::from([to("e.rs", "v.rs", false, Some(&renames))])
    );
}

/// What a user's configuration may set in the repository changes no
/// answer: a signature check, rename following, the root commit's paths,
/// the log's encoding, the rename limit and color are pinned.
#[test]
fn a_users_configuration_changes_no_answer() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    repository(dir, &["a.rs", "gone.rs"]);
    git(dir, &["mv", "a.rs", "a1.rs"]);
    git(dir, &["rm", "--quiet", "gone.rs"]);
    git(dir, &["commit", "--quiet", "-m", "renames"]);
    let clean = (
        renamed(dir, &asked(&["a.rs"])),
        History::of(dir).unwrap().commits(&asked(&["gone.rs"])),
    );
    for setting in [
        "log.showSignature=true",
        "log.follow=true",
        "log.showRoot=false",
        "i18n.logOutputEncoding=UTF-16",
        "diff.renameLimit=1",
        "color.ui=always",
        "color.diff=always",
    ] {
        let (key, value) = setting.split_once('=').unwrap();
        git(dir, &["config", key, value]);
    }

    let hostile = (
        renamed(dir, &asked(&["a.rs"])),
        History::of(dir).unwrap().commits(&asked(&["gone.rs"])),
    );

    assert_eq!(clean.0.len(), 1, "{clean:?}");
    assert_eq!(hostile, clean);
}

/// A chain of renames longer than [`MAX_RENAME_HOPS`] gives no answer,
/// and walks the history once per hop taken, none after the last: a walk
/// checks the paths the last hop reached as it looks for the next.
#[test]
fn a_chain_past_the_hops_walks_the_history_once_per_hop() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    repository(dir, &["f0.rs"]);
    for hop in 0..=MAX_RENAME_HOPS {
        let (from, to) = (format!("f{hop}.rs"), format!("f{}.rs", hop + 1));
        git(dir, &["mv", &from, &to]);
        git(dir, &["commit", "--quiet", "-m", &to]);
    }
    WALKS.with(|walks| walks.set(0));

    let found = renamed(dir, &asked(&["f0.rs"]));

    assert!(found.is_empty(), "{found:?}");
    assert_eq!(WALKS.with(std::cell::Cell::get), MAX_RENAME_HOPS);
}

/// The code root may be a subdirectory of its work tree: paths are asked
/// and answered relative to it, and a rename out of it is no answer.
#[test]
fn paths_are_relative_to_the_code_root() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    repository(dir, &["sub/a.rs", "sub/b.rs"]);
    fs::create_dir(dir.join("sub/src")).unwrap();
    git(dir, &["mv", "sub/a.rs", "sub/src/a.rs"]);
    git(dir, &["mv", "sub/b.rs", "b.rs"]);
    git(dir, &["commit", "--quiet", "-m", "moves"]);

    let found = renamed(&dir.join("sub"), &asked(&["a.rs", "b.rs"]));

    let moves = head(dir, "HEAD");
    assert_eq!(
        found,
        BTreeMap::from([to("a.rs", "src/a.rs", true, Some(&moves))])
    );
}

/// A tree with no git has no history to ask, and an unborn branch is
/// asked nothing that answers.
#[test]
fn no_history_gives_no_answer() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    fs::write(dir.join("a1.rs"), BODY).unwrap();
    assert!(History::of(dir).is_none());
    git(dir, &["init", "--quiet", "--initial-branch=main"]);
    assert!(renamed(dir, &asked(&["a.rs"])).is_empty());
}
