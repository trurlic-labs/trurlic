//! The `git` command line, run as a child process: the one way trurlic asks
//! git anything.
//!
//! Every invocation names its directory with `-C` and clears the variables
//! through which the caller's environment can point git at another
//! repository or change what the arguments mean ([`CLEARED`]): trurlic runs
//! inside a project's git hooks, where git exports `GIT_DIR` and
//! `GIT_INDEX_FILE` for the repository that runs the hook. Paths come from
//! `-z` output, or from `rev-parse`, which prints one path and a line
//! break, so no name is quoted or cut at a newline.
//!
//! The questions about the project's repository: which files the index
//! tracks, which an ignore rule matches, the top of a directory's work
//! tree and of the one a git directory serves, the common dir, the
//! worktree list, and its history ([`renames`]). "No work tree" is an
//! answer, `None`, never a parsed message; a missing binary is
//! [`Error::GitMissing`]. Every child's stdout is read to a stated bound
//! ([`MAX_OUTPUT_BYTES`]), and none reads the caller's stdin. None of them
//! runs inside a graph directory: the graph's own repository is
//! [`graph`]'s alone.

pub(crate) mod graph;
pub(crate) mod renames;

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdout, Command, ExitStatus, Output, Stdio};

use crate::{Error, Result};

/// The most bytes of a git command's stdout that are read: past them the
/// command is killed and its call fails with [`Error::GitOutputTooLarge`].
/// The largest output read is the code root's history walk, of
/// at most [`renames::MAX_WALKED_COMMITS`] commits at about 140 bytes for
/// each file a commit changes, or one batch of blobs, which
/// [`renames::blobs`] sizes within this bound; a git that prints without
/// end is cut off here rather than filling memory.
pub(crate) const MAX_OUTPUT_BYTES: u64 = 256 * 1024 * 1024;

/// The bytes of a git command's stderr kept for its error message; the
/// rest is read and dropped, so a command that warns at length neither
/// fills memory nor blocks on a full pipe.
const MAX_STDERR_BYTES: u64 = 64 * 1024;

/// The variables cleared for every invocation, each one a way for an
/// enclosing process to redirect git. The repository, its parts and its
/// refs: `GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE`, `GIT_COMMON_DIR`,
/// `GIT_OBJECT_DIRECTORY`, `GIT_ALTERNATE_OBJECT_DIRECTORIES`,
/// `GIT_QUARANTINE_PATH` and `GIT_NAMESPACE`. The configuration a parent
/// `git -c` or the environment passes down, which can set `core.worktree`
/// among anything else: `GIT_CONFIG_PARAMETERS` and `GIT_CONFIG_COUNT`
/// (without which `GIT_CONFIG_KEY_<n>` is not read). How a pathspec is
/// matched: `GIT_LITERAL_PATHSPECS`, `GIT_GLOB_PATHSPECS`,
/// `GIT_NOGLOB_PATHSPECS` and `GIT_ICASE_PATHSPECS`.
///
/// Kept: the user's and the system's configuration files and the variables
/// that choose them, and `GIT_CEILING_DIRECTORIES`, a user's bound on
/// discovery that git never sets for a child. `GIT_PREFIX` is kept too: git
/// sets it for an alias's command and reads it nowhere.
const CLEARED: [&str; 14] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_QUARANTINE_PATH",
    "GIT_NAMESPACE",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_LITERAL_PATHSPECS",
    "GIT_GLOB_PATHSPECS",
    "GIT_NOGLOB_PATHSPECS",
    "GIT_ICASE_PATHSPECS",
];

/// The paths under `pathspec` that the index of the repository holding
/// `dir` tracks, relative to `dir` as git prints them. Fails when `dir` is
/// in no work tree.
pub(crate) fn tracked(dir: &Path, pathspec: &Path) -> Result<Vec<PathBuf>> {
    let args = [
        "ls-files".as_ref(),
        "-z".as_ref(),
        "--".as_ref(),
        pathspec.as_os_str(),
    ];
    Ok(paths(&run(command(dir), dir, &args, None)?))
}

/// The files under `pathspec` that the index does not hold and an ignore
/// rule matches (`.gitignore`, `info/exclude`, `core.excludesFile`),
/// relative to `dir` as git prints them. A file the index holds is never
/// ignored, whatever rule matches it.
pub(crate) fn ignored(dir: &Path, pathspec: &Path) -> Result<Vec<PathBuf>> {
    let args = [
        "ls-files".as_ref(),
        "-z".as_ref(),
        "--others".as_ref(),
        "--ignored".as_ref(),
        "--exclude-standard".as_ref(),
        "--".as_ref(),
        pathspec.as_os_str(),
    ];
    Ok(paths(&run(command(dir), dir, &args, None)?))
}

/// Whether an ignore rule matches `path`, relative to `dir`, whether or not
/// the file exists: `git check-ignore`, whose exit status is the answer.
pub(crate) fn is_ignored(dir: &Path, path: &Path) -> Result<bool> {
    let args = [
        "check-ignore".as_ref(),
        "--quiet".as_ref(),
        "--".as_ref(),
        path.as_os_str(),
    ];
    let output = finish(command(dir), &args, None)?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        // A fatal error, or a signal.
        _ => Err(failed(dir, &args, &output)),
    }
}

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

/// The stdout of `command` with `args` appended and `input` on its stdin,
/// which must exit with status 0; `dir` is the directory a failure names.
/// Every git call that needs status 0 runs here.
fn run(command: Command, dir: &Path, args: &[&OsStr], input: Option<&[u8]>) -> Result<Vec<u8>> {
    let output = finish(command, args, input)?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(failed(dir, args, &output))
    }
}

/// `git -C <dir>` with [`CLEARED`] removed, for its caller to add to.
fn command(dir: &Path) -> Command {
    let mut command = Command::new("git");
    command.arg("-C").arg(dir);
    for variable in CLEARED {
        command.env_remove(variable);
    }
    command
}

/// `command` with `args` appended, run to its end, whatever its status,
/// with `input` on its stdin, or with stdin null when there is none, so no
/// child reads the caller's own stdin. Its stdout is read to at most
/// [`MAX_OUTPUT_BYTES`]: past them the child is killed and the call fails.
/// No deadline bounds the wait: a git that never returns is the user's
/// environment, as a file system that never returns is, and a deadline
/// would make what a call returns depend on the machine's load.
fn finish(mut command: Command, args: &[&OsStr], input: Option<&[u8]>) -> Result<Output> {
    command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.stdin(match input {
        Some(_) => Stdio::piped(),
        None => Stdio::null(),
    });
    let not_started = |source: std::io::Error| match source.kind() {
        ErrorKind::NotFound => Error::GitMissing,
        // Every other failure to start the process or to collect its output.
        _ => Error::GitSpawn {
            args: shown(args),
            source,
        },
    };
    let mut child = command.spawn().map_err(not_started)?;
    let (status, stdout, stderr) = collect(&mut child, input).map_err(not_started)?;
    match stdout {
        Some(stdout) => Ok(Output {
            status,
            stdout,
            stderr,
        }),
        None => Err(Error::GitOutputTooLarge {
            args: shown(args),
            max: MAX_OUTPUT_BYTES,
        }),
    }
}

/// The status of `child` once it ends, its stdout, `None` past
/// [`MAX_OUTPUT_BYTES`], when it is killed, and the stderr [`kept`] of it,
/// with `input` written to its stdin. The input is written, and the stderr
/// read, each from a thread of its own while this one reads the stdout, so
/// no pipe can fill while another waits. A write git cuts short by exiting
/// is git's failure, which its status reports.
fn collect(
    child: &mut Child,
    input: Option<&[u8]>,
) -> std::io::Result<(ExitStatus, Option<Vec<u8>>, Vec<u8>)> {
    let (stdin, stdout, stderr) = (child.stdin.take(), child.stdout.take(), child.stderr.take());
    std::thread::scope(|scope| {
        if let Some((bytes, mut stdin)) = input.zip(stdin) {
            scope.spawn(move || {
                let _ = stdin.write_all(bytes);
            });
        }
        let stderr = scope.spawn(move || kept(stderr));
        let stdout = bounded(stdout);
        if !matches!(stdout, Ok(Some(_))) {
            // A child that already exited cannot be killed, and needs not be.
            let _ = child.kill();
        }
        let status = child.wait()?;
        let stderr = stderr.join().unwrap_or_default();
        Ok((status, stdout?, stderr))
    })
}

/// What `pipe` gives to its end, `None` past [`MAX_OUTPUT_BYTES`].
fn bounded(pipe: Option<ChildStdout>) -> std::io::Result<Option<Vec<u8>>> {
    let mut bytes = Vec::new();
    if let Some(pipe) = pipe {
        pipe.take(MAX_OUTPUT_BYTES + 1).read_to_end(&mut bytes)?;
    }
    let within = u64::try_from(bytes.len()).is_ok_and(|read| read <= MAX_OUTPUT_BYTES);
    Ok(within.then_some(bytes))
}

/// The first [`MAX_STDERR_BYTES`] of `pipe`, the rest read and dropped: a
/// failure to read leaves what was read.
fn kept(pipe: Option<ChildStderr>) -> Vec<u8> {
    let mut bytes = Vec::new();
    if let Some(mut pipe) = pipe {
        let _ = (&mut pipe).take(MAX_STDERR_BYTES).read_to_end(&mut bytes);
        let _ = std::io::copy(&mut pipe, &mut std::io::sink());
    }
    bytes
}

fn failed(dir: &Path, args: &[&OsStr], output: &Output) -> Error {
    Error::GitFailed {
        dir: dir.to_path_buf(),
        args: shown(args),
        status: output.status,
        stderr: String::from_utf8_lossy(&output.stderr)
            .trim_end()
            .to_owned(),
    }
}

/// `args` as one line, for an error.
fn shown(args: &[&OsStr]) -> String {
    args.iter()
        .map(|arg| arg.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The NUL-terminated paths of `-z` output.
fn paths(stdout: &[u8]) -> Vec<PathBuf> {
    stdout
        .split(|&byte| byte == 0)
        .filter(|path| !path.is_empty())
        .map(path)
        .collect()
}

/// The one path `rev-parse` prints, with its line break: everything but
/// the last byte, so a line break inside the path stays in it.
fn line_path(mut stdout: Vec<u8>) -> PathBuf {
    if stdout.last() == Some(&b'\n') {
        stdout.pop();
    }
    path(&stdout)
}

/// A path as git printed its bytes.
#[cfg(unix)]
fn path(bytes: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    PathBuf::from(OsStr::from_bytes(bytes))
}

/// A path as git printed its bytes, which git for Windows writes as UTF-8.
#[cfg(not(unix))]
fn path(bytes: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
}

/// The bytes git reads for `path` on its stdin.
#[cfg(unix)]
fn path_bytes(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().to_vec()
}

/// The bytes git reads for `path` on its stdin: UTF-8 on Windows.
#[cfg(not(unix))]
fn path_bytes(path: &Path) -> Vec<u8> {
    path.to_string_lossy().into_owned().into_bytes()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use tempfile::TempDir;

    /// `git <args>` in `dir`, isolated from the user's and the system's
    /// configuration.
    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .current_dir(dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    #[test]
    fn tracked_lists_the_indexed_paths_under_the_pathspec() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        git(dir, &["init", "--quiet"]);
        fs::create_dir_all(dir.join("store/sub")).unwrap();
        for file in [
            "store/a b.toml",
            "store/sub/c\nd.toml",
            "store/new.toml",
            "outside.toml",
        ] {
            fs::write(dir.join(file), "").unwrap();
        }
        git(dir, &["add", "store/a b.toml", "store/sub", "outside.toml"]);

        let mut found = tracked(dir, Path::new("store")).unwrap();
        found.sort();

        assert_eq!(
            found,
            [
                PathBuf::from("store/a b.toml"),
                PathBuf::from("store/sub/c\nd.toml")
            ]
        );
    }

    #[test]
    fn a_directory_in_no_work_tree_is_a_failed_command_with_its_stderr() {
        let tmp = TempDir::new().unwrap();

        let err = tracked(tmp.path(), Path::new("store")).unwrap_err();

        let Error::GitFailed {
            dir, args, stderr, ..
        } = &err
        else {
            panic!("{err:?}");
        };
        assert_eq!(dir, tmp.path());
        assert_eq!(args, "ls-files -z -- store");
        assert!(stderr.contains("not a git repository"), "{stderr}");
        assert!(err.to_string().contains("not a git repository"), "{err}");
    }

    /// A repository at `dir/main` with one commit and a linked worktree at
    /// `dir/linked` on branch `side`, both returned canonical.
    fn main_and_linked(dir: &Path) -> (PathBuf, PathBuf) {
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

    #[test]
    fn ignored_lists_untracked_files_a_rule_matches_and_is_ignored_needs_no_file() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        git(dir, &["init", "--quiet"]);
        fs::create_dir_all(dir.join("store/sub")).unwrap();
        for file in ["store/kept.toml", "store/sub/a b.toml", "store/forced.toml"] {
            fs::write(dir.join(file), "").unwrap();
        }
        fs::write(dir.join(".gitignore"), "sub/\nforced.toml\n").unwrap();
        git(dir, &["add", "--force", "store/forced.toml"]);

        assert_eq!(
            ignored(dir, Path::new("store")).unwrap(),
            [PathBuf::from("store/sub/a b.toml")]
        );
        assert!(is_ignored(dir, Path::new("store/sub/new.toml")).unwrap());
        assert!(!is_ignored(dir, Path::new("store/new.toml")).unwrap());
    }

    #[test]
    fn line_path_drops_only_the_last_line_break() {
        assert_eq!(line_path(b"/a\nb\n".to_vec()), PathBuf::from("/a\nb"));
        assert_eq!(line_path(b"/a".to_vec()), PathBuf::from("/a"));
    }

    #[test]
    fn minus_z_output_splits_on_nul_only() {
        assert_eq!(
            paths(b"a\0b c\0d\ne\0"),
            [
                PathBuf::from("a"),
                PathBuf::from("b c"),
                PathBuf::from("d\ne")
            ]
        );
        assert_eq!(paths(b""), Vec::<PathBuf>::new());
    }
}
