//! How `git` is run: the child process, its cleared environment, and the
//! bounds on what it prints. Every query of `git`, those of [`graph`] and
//! [`renames`] included, runs through it.
//!
//! [`graph`]: super::graph
//! [`renames`]: super::renames

use std::ffi::OsStr;
use std::io::{ErrorKind, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStderr, ChildStdout, Command, ExitStatus, Output, Stdio};

use crate::{Error, Result};

/// The most bytes of a git command's stdout that are read: past them the
/// command is killed and its call fails with [`Error::GitOutputTooLarge`].
/// The largest output read is the code root's history walk, of
/// at most [`super::renames::MAX_WALKED_COMMITS`] commits at about 140 bytes for
/// each file a commit changes, or one batch of blobs, which
/// [`super::renames::blobs`] sizes within this bound; a git that prints without
/// end is cut off here rather than filling memory.
pub(super) const MAX_OUTPUT_BYTES: u64 = 256 * 1024 * 1024;

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

/// The stdout of `command` with `args` appended and `input` on its stdin,
/// which must exit with status 0; `dir` is the directory a failure names.
/// Every git call that needs status 0 runs here.
pub(super) fn run(
    command: Command,
    dir: &Path,
    args: &[&OsStr],
    input: Option<&[u8]>,
) -> Result<Vec<u8>> {
    let output = finish(command, args, input)?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(failed(dir, args, &output))
    }
}

/// `git -C <dir>` with [`CLEARED`] removed, for its caller to add to.
pub(super) fn command(dir: &Path) -> Command {
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
pub(super) fn finish(
    mut command: Command,
    args: &[&OsStr],
    input: Option<&[u8]>,
) -> Result<Output> {
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

pub(super) fn failed(dir: &Path, args: &[&OsStr], output: &Output) -> Error {
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
pub(super) fn shown(args: &[&OsStr]) -> String {
    args.iter()
        .map(|arg| arg.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use tempfile::TempDir;

    use super::super::tracked;
    use super::*;

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
}
