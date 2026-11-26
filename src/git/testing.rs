//! The repositories the tests of this module build.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// `git <args>` in `dir`, isolated from the user's and the system's
/// configuration.
pub(super) fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

/// A repository at `dir/main` with one commit and a linked worktree at
/// `dir/linked` on branch `side`, both returned canonical.
pub(super) fn main_and_linked(dir: &Path) -> (PathBuf, PathBuf) {
    let main = dir.join("main");
    fs::create_dir(&main).unwrap();
    git(&main, &["init", "--quiet", "--initial-branch=trunk"]);
    git(
        &main,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t.invalid",
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "first",
        ],
    );
    git(
        &main,
        &["worktree", "add", "--quiet", "-b", "side", "../linked"],
    );
    (
        main.canonicalize().unwrap(),
        dir.join("linked").canonicalize().unwrap(),
    )
}
