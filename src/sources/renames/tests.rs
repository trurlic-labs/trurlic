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

/// The revision where an anchored file last held its code: `HEAD` for one
/// the work tree changed or removed since, else the parent of the last
/// commit that changed it, its deletion for a file gone from `HEAD`; a
/// file the history never held gets none.
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

    let found = revisions(
        dir,
        &asked(&["kept.rs", "edited.rs", "gone.rs", "dirty.rs", "never.rs"]),
    );

    let at = |path: &str, revision: String| (path.to_owned(), revision);
    assert_eq!(
        found,
        BTreeMap::from([
            at("dirty.rs", "HEAD".to_owned()),
            at("edited.rs", format!("{second}^")),
            at("gone.rs", format!("{second}^")),
            at("kept.rs", format!("{first}^")),
        ])
    );
}

/// A chain of renames longer than [`MAX_RENAME_HOPS`] gives no answer,
/// and walks the history once per hop taken, none after the last.
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

/// A tree with no git, and an unborn branch, are asked nothing that
/// answers.
#[test]
fn no_history_gives_no_answer() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    fs::write(dir.join("a1.rs"), BODY).unwrap();
    assert!(renamed(dir, &asked(&["a.rs"])).is_empty());
    git(dir, &["init", "--quiet", "--initial-branch=main"]);
    assert!(renamed(dir, &asked(&["a.rs"])).is_empty());
}
