//! The error every module returns. `trurlic install` alone has its own,
//! `InstallError`, which wraps this one; nothing here names it.
//!
//! A failure that comes from a file names the file: [`Error::Io`] and
//! [`Error::Toml`] carry the path, and nothing converts a bare
//! `io::Error` or TOML error into an [`Error`], so a `?` cannot drop it.

use std::io;
use std::path::{Path, PathBuf};

use crate::store::graph::Issue;
use crate::store::text::{TextFault, TextField};
use crate::store::{FormatVersion, TimestampError};

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

    /// An older format no migration step starts at, so no trurlic wrote it.
    #[error(
        "{}: format version {found} was never written by trurlic, and no migration starts at it",
        path.display()
    )]
    UnknownFormatVersion { path: PathBuf, found: FormatVersion },

    #[error(
        "not a trurlic project (no .trurlic/ found in {0} or any parent directory); \
         run `trurlic init` in the project root"
    )]
    StoreNotFound(PathBuf),

    #[error(".trurlic/ already exists at {0}")]
    StoreExists(PathBuf),

    #[error("could not acquire lock within {timeout_secs}s — {detail}")]
    LockTimeout { timeout_secs: u64, detail: String },

    #[error(
        "invalid name `{0}`: must be kebab-case (lowercase ASCII, digits, hyphens; \
             no leading/trailing/consecutive hyphens)"
    )]
    InvalidName(String),

    #[error("component `{0}` does not exist")]
    ComponentNotFound(String),

    #[error("component `{0}` already exists")]
    ComponentExists(String),

    #[error("decision `{0}` does not exist")]
    DecisionNotFound(String),

    #[error("pattern `{0}` does not exist")]
    PatternNotFound(String),

    #[error("`{0}` is reserved and cannot be used as a node name")]
    ReservedName(String),

    #[error("component `{0}` cannot connect to itself")]
    SelfConnection(String),

    #[error("connection `{from}` \u{2192} `{to}` already exists")]
    DuplicateConnection { from: String, to: String },

    #[error("connection `{from}` \u{2192} `{to}` does not exist")]
    ConnectionNotFound { from: String, to: String },

    #[error(
        "the commit is recorded in {} but not applied ({source}); the next read or write applies it",
        journal.display()
    )]
    CommitPending {
        journal: PathBuf,
        source: std::io::Error,
    },

    #[error(
        "cannot apply the commit recorded in {}: {detail}; delete that file to drop \
         the commit, then run `trurlic check`",
        journal.display()
    )]
    BadJournal { journal: PathBuf, detail: String },

    /// The system clock reads an instant a `Timestamp` cannot hold.
    #[error("system clock: {source}")]
    Clock { source: TimestampError },

    #[error("{0} consistency error(s) found")]
    CheckFailed(usize),

    #[error("{0}")]
    Validation(String),

    /// A decision's choice or reason breaks a rule of `store::text`.
    #[error("`{field}` {fault}")]
    InvalidText { field: TextField, fault: TextFault },

    /// Another decision of the component has the same choice, compared
    /// without case and runs of whitespace.
    #[error(
        "decision `{existing}` in [{component}] already has this choice; revise \
         `{existing}` instead of duplicating it"
    )]
    DuplicateChoice { component: String, existing: String },

    /// The errors a refused write would have added to the graph, in
    /// validation order.
    #[error("graph integrity violation: {}", messages(.0))]
    GraphIntegrity(Vec<Issue>),

    #[error("operation blocked by cascade rule: {0}")]
    CascadeBlocked(String),
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

fn messages(issues: &[Issue]) -> String {
    issues
        .iter()
        .map(|issue| issue.message.as_str())
        .collect::<Vec<_>>()
        .join("; ")
}
