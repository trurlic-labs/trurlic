//! The error every module returns. `trurlic install` alone has its own,
//! `InstallError`, which wraps this one; nothing here names it.
//!
//! A failure that comes from a file names the file: [`Error::Io`] and
//! [`Error::Toml`] carry the path, and nothing converts a bare
//! `io::Error` or TOML error into an [`Error`], so a `?` cannot drop it.

use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;

use crate::store::{
    AnchorFault, FormatVersion, PlacementDoubt, Refusal, TextFault, TextField, TimestampError,
};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// `path` is the file or directory the failed call was given.
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },

    #[error("{}: invalid TOML: {source}", path.display())]
    Toml {
        path: PathBuf,
        source: toml::de::Error,
    },

    /// `path` is the file the value was being serialized for.
    #[error("{}: cannot serialize as TOML: {source}", path.display())]
    TomlSerialize {
        path: PathBuf,
        source: toml::ser::Error,
    },

    /// An I/O call with no file behind it: a standard stream, the working
    /// directory, the path of the running binary. `what` names the call.
    #[error("cannot {what}: {source}")]
    System { what: String, source: io::Error },

    /// `path` is the `project.toml` that states `found`.
    #[error(
        "{}: format version {found} is {} the {expected} this trurlic reads; {}",
        path.display(),
        version_relation(*found, *expected),
        version_fix(*found, *expected)
    )]
    VersionMismatch {
        path: PathBuf,
        found: FormatVersion,
        expected: FormatVersion,
    },

    /// The format version is not a string of the form MAJOR.MINOR.PATCH;
    /// `text` is the value as the file has it.
    #[error("{}: format version `{text}` is not MAJOR.MINOR.PATCH", path.display())]
    BadFormatVersion { path: PathBuf, text: String },

    #[error("{}: no `format_version` key", path.display())]
    MissingFormatVersion { path: PathBuf },

    /// No chain of migration steps leads from `found` to `expected`: no
    /// trurlic wrote `found`, or this one cannot migrate it.
    #[error(
        "{}: no migration of this trurlic leads from format {found} to {expected}",
        path.display()
    )]
    NoMigration {
        path: PathBuf,
        found: FormatVersion,
        expected: FormatVersion,
    },

    /// A node file or `project.toml` larger than a node may be, refused
    /// before it is parsed.
    #[error("{}: larger than the {max} bytes a node file may hold", path.display())]
    NodeTooLarge { path: PathBuf, max: usize },

    /// The canonical text of a value for `path` does not read back as it.
    #[error("{}: the canonical form does not read back as the value written: {detail}", path.display())]
    RoundTrip { path: PathBuf, detail: String },

    /// A file of an older format that the migration steps left in place,
    /// which the current format has no place for.
    #[error(
        "{}: the current format has no place for this file, and no migration step removed it",
        path.display()
    )]
    Unmigrated { path: PathBuf },

    /// The step to format 0.5 writes `placement`, and git does not tell it.
    /// `store` is the store directory.
    #[error(
        "{}: cannot tell whether the project's git versions this directory: {doubt}; \
         state it with `trurlic migrate --placement tracked` when it does, or \
         `--placement shared` when the directory stays out of the project's git",
        store.display()
    )]
    PlacementUnknown {
        store: PathBuf,
        doubt: PlacementDoubt,
    },

    /// Writing each `A constrains B` as `B depends_on A` would close a
    /// `depends_on` cycle among `members`, sorted.
    #[error(
        "writing each `A constrains B` of graph.toml as `B depends_on A` closes a depends_on \
         cycle among {}; remove one of the `constrains` or `depends_on` edges among them from \
         graph.toml, then migrate again",
        quoted(members)
    )]
    ConstrainsCycle { members: Vec<String> },

    #[error("cannot run `git`: it is not installed or not on PATH")]
    GitMissing,

    /// `args` are the arguments after `git -C <dir>`.
    #[error("cannot run `git {args}`: {source}")]
    GitSpawn { args: String, source: io::Error },

    /// `stderr` is git's, without its trailing line break.
    #[error("`git {args}` in {} failed ({status}): {stderr}", dir.display())]
    GitFailed {
        dir: PathBuf,
        args: String,
        status: ExitStatus,
        stderr: String,
    },

    /// Git exited with status 0 and printed what the runner cannot read.
    #[error("`git {args}` printed {detail}")]
    GitOutput { args: String, detail: &'static str },

    /// A symlink named `.trurlic` that leads to nothing: its target is
    /// gone, or the links loop.
    #[error(
        "{}: the symlink to the graph leads nowhere: {source}; point it at a graph directory or remove it",
        link.display()
    )]
    GraphLinkBroken { link: PathBuf, source: io::Error },

    /// A `.trurlic` that is a file, or a symlink to one.
    #[error(
        "{}: not a directory; a graph is a directory named .trurlic, or a symlink to one",
        path.display()
    )]
    GraphNotDirectory { path: PathBuf },

    /// `init --placement tracked` where no git work tree holds `dir`.
    #[error(
        "placement tracked needs the project's git, and no git work tree holds {}; run \
         `git init` first, or `trurlic init --placement shared` to keep the graph out of git",
        dir.display()
    )]
    NoWorkTree { dir: PathBuf },

    /// `init --placement tracked` where an ignore rule matches the graph.
    #[error(
        "git ignores {}, so it cannot version a tracked graph; remove the rule that \
         matches it, or run `trurlic init --placement shared`",
        graph.display()
    )]
    GraphIgnored { graph: PathBuf },

    /// `init --placement shared` in a work tree, away from the top of its
    /// repository's main worktree, `main`.
    #[error(
        "a shared graph lives at the top of the main worktree, {}, where every linked \
         worktree finds it; run `trurlic init --placement shared` there, not in {}",
        main.display(),
        dir.display()
    )]
    SharedOffMain { dir: PathBuf, main: PathBuf },

    /// `init --placement shared` in a worktree of a bare repository.
    #[error(
        "the repository of {} is bare: it has no main worktree to hold a shared graph",
        dir.display()
    )]
    SharedInBare { dir: PathBuf },

    /// `init` in a linked worktree that the main worktree's shared graph
    /// serves already.
    #[error(
        "the shared graph {} of the main worktree serves this worktree; a second graph \
         beside it is refused",
        graph.display()
    )]
    SharedGraphServes { graph: PathBuf },

    /// A path given to a query that could name a place outside the code root.
    #[error("invalid path `{path}`: {fault}")]
    InvalidPath { path: String, fault: AnchorFault },

    #[error(
        "not a trurlic project (no .trurlic/ in {0} or any parent directory, and no \
         shared graph in the main worktree of its repository); run \
         `trurlic init --placement tracked|shared` in the project root"
    )]
    StoreNotFound(PathBuf),

    #[error(".trurlic/ already exists at {0}")]
    StoreExists(PathBuf),

    #[error("could not acquire lock within {timeout_secs}s — {detail}")]
    LockTimeout { timeout_secs: u64, detail: String },

    /// A failure after the commit point: the journal holds the write, and
    /// the next opener finishes it, its history commit included.
    #[error(
        "the write is recorded in {} but not finished ({source}); the next read or write \
         finishes it",
        journal.display()
    )]
    WritePending {
        journal: PathBuf,
        source: Box<Error>,
    },

    /// A shared graph's write found the lock git holds while it writes the
    /// index of the graph's repository, and wrote nothing.
    #[error(
        "{}: git holds the index of the graph's repository, or a git process that stopped \
         left this lock; the write is refused: remove the lock once no git process runs there",
        lock.display()
    )]
    GraphIndexLocked { lock: PathBuf },

    /// An `Origin:` or `Session:` for a history commit that breaks the
    /// one-line rule of `store::text`.
    #[error("`{field}` {fault}")]
    TrailerText { field: TextField, fault: TextFault },

    #[error(
        "cannot apply the commit recorded in {}: {detail}; delete that file to drop \
         the commit, then run `trurlic check`",
        journal.display()
    )]
    BadJournal { journal: PathBuf, detail: String },

    /// The system clock reads an instant a `Timestamp` cannot hold.
    #[error("system clock: {source}")]
    Clock { source: TimestampError },

    /// A batch of `apply` the store refused whole, with nothing written.
    #[error("{0}")]
    Refused(Box<Refusal>),

    /// A `[quality]` pattern of the `project.toml` at `path` that does not
    /// compile, or compiles past its size limit; `message` is regex's.
    #[error(
        "{}: `[quality] patterns[{index}].pattern` does not compile: {message}",
        path.display()
    )]
    QualityPattern {
        path: PathBuf,
        index: usize,
        message: String,
    },

    #[error("{0} consistency error(s) found")]
    CheckFailed(usize),

    #[error("{0}")]
    Validation(String),
}

impl Error {
    /// The `map_err` adapter for an I/O call on `path`.
    pub(crate) fn io(path: &Path) -> impl FnOnce(io::Error) -> Self + '_ {
        |source| Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }

    /// The `map_err` adapter for an I/O call that `what` names, such as
    /// "write to stdout".
    pub(crate) fn system(what: impl Into<String>) -> impl FnOnce(io::Error) -> Self {
        |source| Self::System {
            what: what.into(),
            source,
        }
    }

    /// The `map_err` adapter for parsing the TOML read from `path`.
    pub(crate) fn toml(path: &Path) -> impl FnOnce(toml::de::Error) -> Self + '_ {
        |source| Self::Toml {
            path: path.to_path_buf(),
            source,
        }
    }
}

/// `names`, each in backticks, comma-separated.
fn quoted(names: &[String]) -> String {
    let quoted: Vec<String> = names.iter().map(|name| format!("`{name}`")).collect();
    quoted.join(", ")
}

fn version_relation(found: FormatVersion, expected: FormatVersion) -> &'static str {
    if found > expected {
        "newer than"
    } else {
        "older than"
    }
}

fn version_fix(found: FormatVersion, expected: FormatVersion) -> &'static str {
    if found > expected {
        "upgrade trurlic"
    } else {
        "run `trurlic migrate` to upgrade the store"
    }
}
