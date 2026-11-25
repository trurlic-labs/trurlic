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

    // The commit `renames` made the first rename of each committed path.
    let first = format!("{}^", head(dir, "HEAD~1"));
    assert_eq!(
        found,
        BTreeMap::from([
            to("a.rs", "a1.rs", true, &first),
            to("b.rs", "src/b2.rs", true, &first),
            to("e.rs", "e1.rs", false, &first),
            to("u.rs", "u1.rs", true, "HEAD"),
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

fn to(from: &str, to: &str, exact: bool, before: &str) -> (String, Renamed) {
    let renamed = Renamed {
        to: to.to_owned(),
        exact,
        before: before.to_owned(),
    };
    (from.to_owned(), renamed)
}

/// Where an anchored file last held its code: the work tree for one it
/// changed or removed since `HEAD`, else before the last commit that
/// changed it, its deletion for a file gone from `HEAD`; a file the
/// history never held gets none.
#[test]
fn a_file_is_judged_at_the_revision_before_it_last_changed() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    repository(dir, &["kept.rs", "edited.rs", "gone.rs", "dirty.rs"]);
    fs::write(dir.join("edited.rs"), "fn changed() {}\n").unwrap();
    git(dir, &["rm", "--quiet", "gone.rs"]);
    git(dir, &["commit", "--quiet", "-a", "-m", "second"]);
    fs::write(dir.join("dirty.rs"), "").unwrap();
    let (first, second) = (head(dir, "HEAD~1"), head(dir, "HEAD"));

    let found = History::of(dir).unwrap().judged(&asked(&[
        "kept.rs",
        "edited.rs",
        "gone.rs",
        "dirty.rs",
        "never.rs",
    ]));

    let at = |path: &str, judged: Judged| (path.to_owned(), judged);
    assert_eq!(
        found,
        BTreeMap::from([
            at("dirty.rs", Judged::Uncommitted),
            at("edited.rs", Judged::Before(second.clone())),
            at("gone.rs", Judged::Before(second)),
            at("kept.rs", Judged::Before(first)),
        ])
    );
}

/// A gone file a rename's walk asked about is judged from that walk, with
/// no other.
#[test]
fn a_gone_file_is_judged_by_the_walk_that_looked_for_its_rename() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    repository(dir, &["gone.rs"]);
    git(dir, &["rm", "--quiet", "gone.rs"]);
    git(dir, &["commit", "--quiet", "-m", "gone"]);
    let mut history = History::of(dir).unwrap();
    assert!(history.renamed(&asked(&["gone.rs"])).is_empty());
    WALKS.with(|walks| walks.set(0));

    let found = history.judged(&asked(&["gone.rs"]));

    assert_eq!(WALKS.with(std::cell::Cell::get), 0);
    let deleted = head(dir, "HEAD");
    assert_eq!(
        found,
        BTreeMap::from([("gone.rs".to_owned(), Judged::Before(deleted))])
    );
}

/// A rename counts only while its new path is the file it made: a path
/// the rename reached and that a later commit deleted and wrote again, at
/// the end of the chain or on the way, is no answer.
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

    assert!(found.is_empty(), "{found:?}");
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
        History::of(dir).unwrap().judged(&asked(&["gone.rs"])),
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
        History::of(dir).unwrap().judged(&asked(&["gone.rs"])),
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

    let before = format!("{}^", head(dir, "HEAD"));
    assert_eq!(
        found,
        BTreeMap::from([to("a.rs", "src/a.rs", true, &before)])
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

/// The commits since a revision that changed how often a file spells a
/// name, newest first, and the paths each of some commits changed.
#[test]
fn a_files_spelling_of_a_name_and_a_commits_paths_are_read() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    repository(dir, &["a.rs", "b.rs"]);
    fs::write(
        dir.join("b.rs"),
        "fn helper() {}
",
    )
    .unwrap();
    fs::write(
        dir.join("a.rs"),
        "fn other() {}
",
    )
    .unwrap();
    git(dir, &["commit", "--quiet", "-a", "-m", "gain"]);
    fs::write(
        dir.join("b.rs"),
        "fn helper() { 1 }
",
    )
    .unwrap();
    git(dir, &["commit", "--quiet", "-a", "-m", "edit"]);
    let (first, gain) = (head(dir, "HEAD~2"), head(dir, "HEAD~1"));
    let history = History::of(dir).unwrap();

    let spelled = history.spelling_changes(&first, "b.rs", "helper");
    let changed = history.changed_by(&BTreeSet::from([gain.as_str(), first.as_str()]));

    assert_eq!(spelled, [gain.as_str()]);
    let paths = |names: &[&str]| names.iter().map(|name| (*name).to_owned()).collect();
    assert_eq!(
        changed,
        BTreeMap::from([
            (gain, paths(&["a.rs", "b.rs"])),
            (first, paths(&["a.rs", "b.rs"])),
        ])
    );
}
