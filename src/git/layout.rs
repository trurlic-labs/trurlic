//! The layout of a repository: the first `.git` upward, the top of a work
//! tree, the git directories and the list of worktrees.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::{Error, Result};

use super::encoding::{line_path, path};
use super::process::{command, run, shown};

/// The first `.git` that git's discovery meets from a directory upward.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DotGit {
    /// None where it was looked for; git is not asked.
    Absent,
    /// A directory: a main worktree, or a repository with no other worktree.
    Directory,
    /// A file naming the git directory elsewhere: a linked worktree, a
    /// submodule, or a repository made with `--separate-git-dir`.
    File,
}

/// The first `.git` in `dir` or a directory above it, symlinks followed.
///
/// Git reports "not a git repository" as it reports every fatal error,
/// with status 128 and a translated message, so the runner tells that case
/// from the absence of a `.git` where git's discovery looks for one, and
/// asks git only when there is one; a failure then is git's error.
pub(crate) fn dot_git(dir: &Path) -> Result<DotGit> {
    let dir = dir.canonicalize().map_err(Error::io(dir))?;
    for ancestor in dir.ancestors() {
        match dot_git_in(ancestor)? {
            DotGit::Absent => {}
            found @ (DotGit::Directory | DotGit::File) => return Ok(found),
        }
    }
    Ok(DotGit::Absent)
}

/// The `.git` in `dir` itself, symlinks followed: one metadata call, no
/// process.
pub(crate) fn dot_git_in(dir: &Path) -> Result<DotGit> {
    let path = dir.join(".git");
    match fs::metadata(&path) {
        Ok(metadata) if metadata.is_dir() => Ok(DotGit::Directory),
        Ok(_) => Ok(DotGit::File),
        // A dangling symlink is no `.git` to git either.
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(DotGit::Absent),
        Err(source) => Err(Error::Io { path, source }),
    }
}

/// The top of the work tree that holds `dir`, or `None` when no work tree
/// does: no `.git` is at or above `dir`, or `dir` is in a git directory or
/// a bare repository.
pub(crate) fn toplevel(dir: &Path) -> Result<Option<PathBuf>> {
    if dot_git(dir)? == DotGit::Absent {
        return Ok(None);
    }
    let args = ["rev-parse".as_ref(), "--show-toplevel".as_ref()];
    let failure = match run(command(dir), dir, &args, None) {
        Ok(stdout) => return Ok(Some(line_path(stdout))),
        Err(failure @ Error::GitFailed { .. }) => failure,
        Err(e) => return Err(e),
    };
    // `--show-toplevel` fails outside a work tree as it fails for any other
    // reason; `--is-inside-work-tree` answers `false` there instead.
    let args = ["rev-parse".as_ref(), "--is-inside-work-tree".as_ref()];
    let inside = run(command(dir), dir, &args, None);
    match inside {
        Ok(answer) if answer == b"false\n" => Ok(None),
        _ => Err(failure),
    }
}

/// The git directory every worktree of the repository holding `dir`
/// shares: `.git` of the main worktree, or the bare repository. Git prints
/// it relative to `dir` when it is below it, so a relative answer is joined
/// to `dir`.
pub(crate) fn common_dir(dir: &Path) -> Result<PathBuf> {
    rev_parse_path(dir, "--git-common-dir")
}

/// The git directory of the worktree holding `dir`: the common dir for the
/// main worktree, its own under `worktrees/` for a linked one; joined to
/// `dir` as [`common_dir`] is.
pub(crate) fn git_dir(dir: &Path) -> Result<PathBuf> {
    rev_parse_path(dir, "--git-dir")
}

/// The top of the work tree the git directory `git_dir` serves, as its
/// `core.worktree` states it, which is how git lists a submodule's main
/// worktree by its git directory: `git --git-dir=. rev-parse
/// --show-toplevel` run in that directory. `None` when the configuration
/// states none, so git takes the directory itself for the top.
pub(crate) fn work_tree_of(git_dir: &Path) -> Result<Option<PathBuf>> {
    let args = [
        "--git-dir=.".as_ref(),
        "rev-parse".as_ref(),
        "--show-toplevel".as_ref(),
    ];
    let top = line_path(run(command(git_dir), git_dir, &args, None)?);
    let own = git_dir.canonicalize().map_err(Error::io(git_dir))?;
    let top = top.canonicalize().map_err(Error::io(&top))?;
    Ok((top != own).then_some(top))
}

/// The path `git rev-parse <flag>` prints in `dir`, joined to `dir` when
/// git prints it relative.
fn rev_parse_path(dir: &Path, flag: &str) -> Result<PathBuf> {
    let args = ["rev-parse".as_ref(), flag.as_ref()];
    let path = line_path(run(command(dir), dir, &args, None)?);
    Ok(if path.is_relative() {
        dir.join(path)
    } else {
        path
    })
}

/// One worktree of a repository, as `git worktree list` prints it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Worktree {
    pub(crate) path: PathBuf,
    pub(crate) head: Head,
    /// The commit its HEAD names; `None` on an unborn branch and for a
    /// bare repository.
    pub(crate) commit: Option<String>,
    /// Git marks it prunable: its directory is gone.
    pub(crate) prunable: bool,
}

/// What a worktree has checked out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Head {
    /// The entry is the bare repository itself, which has no work tree.
    Bare,
    /// The branch's name, without `refs/heads/`; a name that is not UTF-8
    /// is read lossily, since it is only shown.
    Branch(String),
    Detached,
}

/// Every worktree of the repository holding `dir`, the main worktree (or
/// the bare repository) first, as git lists them from the common dir.
pub(crate) fn worktrees(dir: &Path) -> Result<Vec<Worktree>> {
    let args = [
        "worktree".as_ref(),
        "list".as_ref(),
        "--porcelain".as_ref(),
        "-z".as_ref(),
    ];
    let stdout = run(command(dir), dir, &args, None)?;
    parse_worktrees(&stdout).ok_or_else(|| Error::GitOutput {
        args: shown(&args),
        detail: "a record that does not start with `worktree <path>`",
    })
}

/// The records of `worktree list --porcelain -z`: NUL-terminated
/// attributes, an empty one after each record. `None` when a record does
/// not start with its path. Attributes this module does not read, such as
/// `locked`, are skipped, so a newer git's are too; a `HEAD` of zeros is an
/// unborn branch's.
fn parse_worktrees(stdout: &[u8]) -> Option<Vec<Worktree>> {
    let mut worktrees = Vec::new();
    let mut fields = stdout.split(|&byte| byte == 0);
    while let Some(first) = fields.next() {
        if first.is_empty() {
            continue;
        }
        let mut worktree = Worktree {
            path: path(first.strip_prefix(b"worktree ")?),
            head: Head::Detached,
            commit: None,
            prunable: false,
        };
        for field in fields.by_ref().take_while(|field| !field.is_empty()) {
            if let Some(commit) = field.strip_prefix(b"HEAD ") {
                let unborn = commit.iter().all(|&byte| byte == b'0');
                worktree.commit = (!unborn).then(|| String::from_utf8_lossy(commit).into_owned());
            } else if field == b"bare" {
                worktree.head = Head::Bare;
            } else if let Some(branch) = field.strip_prefix(b"branch ") {
                let name = branch.strip_prefix(b"refs/heads/").unwrap_or(branch);
                worktree.head = Head::Branch(String::from_utf8_lossy(name).into_owned());
            } else if field == b"prunable" || field.starts_with(b"prunable ") {
                worktree.prunable = true;
            }
        }
        worktrees.push(worktree);
    }
    (!worktrees.is_empty()).then_some(worktrees)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::super::testing::{git, main_and_linked};
    use super::*;

    #[test]
    fn toplevel_names_the_work_tree_of_a_directory_and_none_outside_one() {
        let tmp = TempDir::new().unwrap();
        let (main, linked) = main_and_linked(tmp.path());
        fs::create_dir(linked.join("sub")).unwrap();
        fs::create_dir(tmp.path().join("plain")).unwrap();
        git(tmp.path(), &["init", "--quiet", "--bare", "bare.git"]);

        assert_eq!(toplevel(&main).unwrap(), Some(main.clone()));
        assert_eq!(toplevel(&linked.join("sub")).unwrap(), Some(linked));
        assert_eq!(toplevel(&main.join(".git/refs")).unwrap(), None);
        assert_eq!(toplevel(&tmp.path().join("bare.git")).unwrap(), None);
        assert_eq!(toplevel(&tmp.path().join("plain")).unwrap(), None);
    }

    #[test]
    fn a_dot_git_that_git_refuses_is_its_failure_not_no_work_tree() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join(".git"), "gitdir: /nowhere/at/all\n").unwrap();

        assert_eq!(dot_git(tmp.path()).unwrap(), DotGit::File);
        let err = toplevel(tmp.path()).unwrap_err();
        assert!(
            matches!(&err, Error::GitFailed { args, .. } if args == "rev-parse --show-toplevel"),
            "{err:?}"
        );
    }

    #[test]
    fn dot_git_is_the_first_one_upward() {
        let tmp = TempDir::new().unwrap();
        let (main, linked) = main_and_linked(tmp.path());
        let nested = linked.join("nested");
        fs::create_dir_all(nested.join(".git")).unwrap();

        assert_eq!(dot_git(&main).unwrap(), DotGit::Directory);
        assert_eq!(dot_git(&linked).unwrap(), DotGit::File);
        assert_eq!(dot_git(&nested).unwrap(), DotGit::Directory);
    }

    #[test]
    fn common_dir_is_absolute_from_the_main_and_a_linked_worktree() {
        let tmp = TempDir::new().unwrap();
        let (main, linked) = main_and_linked(tmp.path());

        for dir in [&main, &linked] {
            let common = common_dir(dir).unwrap();
            assert!(common.is_absolute(), "{}", common.display());
            assert_eq!(common.canonicalize().unwrap(), main.join(".git"));
        }
    }

    #[test]
    fn worktrees_lists_the_main_worktree_first_with_each_head() {
        let tmp = TempDir::new().unwrap();
        let (main, linked) = main_and_linked(tmp.path());
        git(
            &main,
            &["worktree", "add", "--quiet", "--detach", "../detached"],
        );
        git(
            &main,
            &["worktree", "add", "--quiet", "-b", "gone", "../gone"],
        );
        fs::remove_dir_all(tmp.path().join("gone")).unwrap();

        let listed = worktrees(&linked).unwrap();

        let canonical = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let mut summary: Vec<(PathBuf, Head, bool)> = listed
            .into_iter()
            .map(|w| (canonical(&w.path), w.head, w.prunable))
            .collect();
        // Git lists the linked worktrees in no order it documents.
        summary[1..].sort_by(|a, b| a.0.cmp(&b.0));
        let tmp_dir = tmp.path().canonicalize().unwrap();
        assert_eq!(
            summary,
            [
                (main, Head::Branch("trunk".into()), false),
                (tmp_dir.join("detached"), Head::Detached, false),
                (tmp_dir.join("gone"), Head::Branch("gone".into()), true),
                (linked, Head::Branch("side".into()), false),
            ]
        );
    }

    #[test]
    fn a_bare_repository_lists_itself_first_as_bare() {
        let tmp = TempDir::new().unwrap();
        git(tmp.path(), &["init", "--quiet", "--bare", "bare.git"]);

        let listed = worktrees(&tmp.path().join("bare.git")).unwrap();

        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].head, Head::Bare);
    }

    #[test]
    fn worktree_records_are_read_as_bytes_and_unknown_attributes_skipped() {
        let stdout = b"worktree /a\nb\0HEAD 01\0branch refs/heads/x\0locked why\0\0\
                       worktree /c\0bare\0\0worktree /d\0HEAD 0000\0detached\0prunable gone\0\
                       future\0\0";

        assert_eq!(
            parse_worktrees(stdout).unwrap(),
            [
                Worktree {
                    path: PathBuf::from("/a\nb"),
                    head: Head::Branch("x".into()),
                    commit: Some("01".into()),
                    prunable: false,
                },
                Worktree {
                    path: PathBuf::from("/c"),
                    head: Head::Bare,
                    commit: None,
                    prunable: false,
                },
                Worktree {
                    path: PathBuf::from("/d"),
                    head: Head::Detached,
                    commit: None,
                    prunable: true,
                },
            ]
        );
        assert_eq!(parse_worktrees(b"HEAD 01\0\0"), None);
        assert_eq!(parse_worktrees(b""), None);
    }
}
