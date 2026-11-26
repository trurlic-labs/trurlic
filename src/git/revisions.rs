//! The questions about commits: the merge base, `HEAD`, and what changed in
//! a work tree since a commit.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::path::Path;

use crate::{Error, Result};

use super::process::{command, failed, finish, run, shown};

/// The best common ancestor of commits `ours` and `theirs`, in the
/// repository holding `dir`: `git merge-base`, `None` when they share no
/// history. Each is a commit id git printed.
pub(crate) fn merge_base(dir: &Path, ours: &str, theirs: &str) -> Result<Option<String>> {
    let args = ["merge-base".as_ref(), ours.as_ref(), theirs.as_ref()];
    let output = finish(command(dir), &args, None)?;
    match output.status.code() {
        Some(0) => match std::str::from_utf8(&output.stdout) {
            Ok(base) => Ok(Some(base.trim_end().to_owned())),
            Err(_) => Err(Error::GitOutput {
                args: shown(&args),
                detail: "a commit id that is not UTF-8",
            }),
        },
        // No common ancestor: the exit status says so, with no output.
        Some(1) if output.stdout.is_empty() => Ok(None),
        // A commit the repository lacks, a fatal error, or a signal.
        _ => Err(failed(dir, &args, &output)),
    }
}

/// The commit `HEAD` names in the repository holding `dir`.
pub(crate) fn head(dir: &Path) -> Result<String> {
    let args = ["rev-parse".as_ref(), "--verify".as_ref(), "HEAD".as_ref()];
    let stdout = run(command(dir), dir, &args, None)?;
    String::from_utf8(stdout)
        .map(|id| id.trim_end().to_owned())
        .map_err(|_| Error::GitOutput {
            args: shown(&args),
            detail: "a commit id that is not UTF-8",
        })
}

/// Which of `paths`, relative to `dir`, differ in the work tree at `dir`
/// from commit `base`: committed there since, staged or edited and not
/// committed, or untracked and not ignored. `git diff --name-only`, no
/// rename paired, then `git ls-files --others --exclude-standard`, each
/// with every path a literal pathspec and without optional locks; a path
/// that is not UTF-8 names no anchor and is left out.
pub(crate) fn changed_since(dir: &Path, base: &str, paths: &[&str]) -> Result<BTreeSet<String>> {
    let pathspecs: Vec<String> = paths
        .iter()
        .map(|path| format!(":(literal){path}"))
        .collect();
    let diff = [
        "--no-optional-locks",
        "diff",
        "--name-only",
        "-z",
        "--relative",
        "--no-renames",
        "--no-ext-diff",
        "--no-textconv",
        base,
        "--",
    ];
    let untracked = [
        "--no-optional-locks",
        "ls-files",
        "-z",
        "--others",
        "--exclude-standard",
        "--",
    ];
    let mut changed = BTreeSet::new();
    for options in [&diff[..], &untracked[..]] {
        let mut args: Vec<&OsStr> = options.iter().map(OsStr::new).collect();
        args.extend(pathspecs.iter().map(OsStr::new));
        let stdout = run(command(dir), dir, &args, None)?;
        let listed = stdout
            .split(|&byte| byte == 0)
            .filter_map(|path| std::str::from_utf8(path).ok())
            .filter(|path| !path.is_empty())
            .map(str::to_owned);
        changed.extend(listed);
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::super::testing::{git, main_and_linked};
    use super::super::worktrees;
    use super::*;

    /// The commit each worktree's HEAD names; the fork of two lines is
    /// their merge base, the older commit itself when one holds the other;
    /// and what a work tree changed since a commit, uncommitted edits and
    /// untracked files included, ignored files and other paths left out.
    #[test]
    fn worktrees_name_their_commits_and_the_fork_is_asked_of_the_graph() {
        let tmp = TempDir::new().unwrap();
        let (main, linked) = main_and_linked(tmp.path());
        let identity = ["-c", "user.name=t", "-c", "user.email=t@t.invalid"];
        fs::write(linked.join("a.rs"), "").unwrap();
        git(&linked, &["add", "a.rs"]);
        let mut commit = identity.to_vec();
        commit.extend(["commit", "--quiet", "-m", "second"]);
        git(&linked, &commit);
        fs::write(linked.join("b.rs"), "").unwrap();
        git(&linked, &["add", "b.rs"]);
        let listed = worktrees(&main).unwrap();
        let (first, second) = (
            listed[0].commit.clone().unwrap(),
            listed[1].commit.clone().unwrap(),
        );

        assert_ne!(first, second);
        assert_eq!(
            merge_base(&main, &first, &second).unwrap(),
            Some(first.clone())
        );
        assert_eq!(
            merge_base(&main, &second, &first).unwrap(),
            Some(first.clone())
        );
        let unknown = "1".repeat(40);
        assert!(matches!(
            merge_base(&main, &unknown, &first),
            Err(Error::GitFailed { .. })
        ));
        fs::write(linked.join("new.rs"), "").unwrap();
        fs::write(linked.join(".gitignore"), "/ignored.rs\n").unwrap();
        fs::write(linked.join("ignored.rs"), "").unwrap();
        let asked = ["a.rs", "b.rs", "c.rs", "new.rs", "ignored.rs"];
        let changed = changed_since(&linked, &first, &asked).unwrap();
        assert_eq!(
            changed,
            BTreeSet::from(["a.rs".to_owned(), "b.rs".to_owned(), "new.rs".to_owned()])
        );
    }
}
