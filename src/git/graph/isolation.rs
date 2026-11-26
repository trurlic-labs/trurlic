//! What every git call in the graph's repository runs under: no
//! configuration of the system's or the user's, the repository's own
//! settings that run code or change bytes overridden ([`OVERRIDES`]), its
//! own attributes ([`ATTRIBUTES`]), a fixed identity ([`NAME`]) and the
//! dates git reads from the clock ([`isolated`]).

use std::path::Path;
use std::process::Command;

use super::super::process::command;

/// The author and committer of every commit: a program, at an address no
/// mailbox can have (`.invalid` is reserved for that).
const NAME: &str = "trurlic";
const EMAIL: &str = "trurlic@trurlic.invalid";

/// The empty file, for a setting or a variable that names a file or the
/// hooks directory.
#[cfg(unix)]
const NULL: &str = "/dev/null";
#[cfg(not(unix))]
const NULL: &str = "NUL";

/// Settings passed with `-c`, which beat the repository's own
/// configuration, the one the environment does not shut out. `{null}` is
/// [`NULL`].
const OVERRIDES: [&str; 9] = [
    // No hook runs, `.git/hooks/` included: `--no-verify` skips only
    // pre-commit and commit-msg, not prepare-commit-msg or post-commit.
    "core.hooksPath={null}",
    // A command `git status` would run.
    "core.fsmonitor=false",
    // Read from `$XDG_CONFIG_HOME/git/` when no configuration names them.
    "core.excludesFile={null}",
    "core.attributesFile={null}",
    // `git status` compares the bytes on disk with the index unconverted,
    // and an exec bit is no change: every graph file is committed 100644.
    "core.autocrlf=false",
    "core.fileMode=false",
    "commit.gpgSign=false",
    // Objects and refs reach the disk before the journal that names them
    // is written, and before the journal that would make the commit again
    // is deleted.
    "core.fsync=committed",
    // The loose objects of one `update-index`, and the trees of one
    // commit, reach the device with one hardware flush before any of them
    // gets its name (git 2.37: `bulk-checkin.c`, `flush_batch_fsync`); a
    // command with no object transaction, or a git before 2.37, which
    // warns and ignores the value, flushes each object on its own.
    "core.fsyncMethod=batch",
];

/// Set for every call. The system's and the user's configuration are read
/// from no file, and the identity is fixed: these variables beat every
/// configuration key that names an author or a committer.
const ENVIRONMENT: [(&str, &str); 6] = [
    ("GIT_CONFIG_NOSYSTEM", "1"),
    ("GIT_CONFIG_GLOBAL", NULL),
    ("GIT_AUTHOR_NAME", NAME),
    ("GIT_AUTHOR_EMAIL", EMAIL),
    ("GIT_COMMITTER_NAME", NAME),
    ("GIT_COMMITTER_EMAIL", EMAIL),
];

/// Removed for every call: a date the caller set would stand in for the
/// clock git reads.
const DATES: [&str; 2] = ["GIT_AUTHOR_DATE", "GIT_COMMITTER_DATE"];

/// The repository's `info/attributes`, which beats every `.gitattributes`
/// and `core.attributesFile`: no attribute that converts bytes applies, so
/// `update-index` writes, and `git status` and `git commit` read, the
/// bytes on disk as they are. With `text` unset, `eol` converts nothing
/// either; it is unset all the same.
pub(super) const ATTRIBUTES: &str = "* -text -eol -filter -ident -working-tree-encoding\n";

/// `git -C <dir>` reading no system or user configuration, with
/// [`OVERRIDES`], [`ENVIRONMENT`], and no [`DATES`].
pub(super) fn isolated(dir: &Path) -> Command {
    let mut command = command(dir);
    for setting in OVERRIDES {
        command.arg("-c").arg(setting.replace("{null}", NULL));
    }
    command.envs(ENVIRONMENT);
    for variable in DATES {
        command.env_remove(variable);
    }
    command
}
