//! The `git` command line, run as a child process: the one way trurlic asks
//! git anything.
//!
//! Every invocation names its directory with `-C` and clears the variables
//! through which the caller's environment can point git at another
//! repository or change what the arguments mean ([`CLEARED`]): trurlic runs
//! inside a project's git hooks, where git exports `GIT_DIR` and
//! `GIT_INDEX_FILE` for the repository that runs the hook. Paths come from
//! `-z` output, so no name is quoted or cut at a newline.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::{Error, Result};

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
    Ok(paths(&run(dir, &args)?))
}

/// The stdout of `git -C <dir> <args>`, which must exit with status 0.
fn run(dir: &Path, args: &[&OsStr]) -> Result<Vec<u8>> {
    let mut command = Command::new("git");
    command.arg("-C").arg(dir).args(args).stdin(Stdio::null());
    for variable in CLEARED {
        command.env_remove(variable);
    }
    let shown = || {
        args.iter()
            .map(|arg| arg.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ")
    };
    let output = command.output().map_err(|source| match source.kind() {
        std::io::ErrorKind::NotFound => Error::GitMissing,
        // Every other reason the process did not start.
        _ => Error::GitSpawn {
            args: shown(),
            source,
        },
    })?;
    if !output.status.success() {
        return Err(Error::GitFailed {
            dir: dir.to_path_buf(),
            args: shown(),
            status: output.status,
            stderr: String::from_utf8_lossy(&output.stderr)
                .trim_end()
                .to_owned(),
        });
    }
    Ok(output.stdout)
}

/// The NUL-terminated paths of `-z` output.
fn paths(stdout: &[u8]) -> Vec<PathBuf> {
    stdout
        .split(|&byte| byte == 0)
        .filter(|path| !path.is_empty())
        .map(path)
        .collect()
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
