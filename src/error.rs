use std::path::PathBuf;

use crate::commands::InstallError;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("invalid TOML: {0}")]
    TomlRead(#[from] toml::de::Error),

    #[error("TOML serialization error: {0}")]
    TomlWrite(#[from] toml::ser::Error),

    #[error("not a trurlic project (no .trurlic/ found in {0} or any parent directory)")]
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
        "the graph was loaded at generation {loaded}, but another commit has \
         raised the store to {on_disk}; reload it under the lock before writing"
    )]
    StaleState { loaded: u64, on_disk: u64 },

    #[error(
        "the commit is recorded in {} but not applied ({source}); the next write applies it",
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

    #[error("{0} consistency error(s) found")]
    CheckFailed(usize),

    #[error("{0}")]
    Validation(String),

    #[error("graph integrity violation: {0}")]
    GraphIntegrity(String),

    #[error("operation blocked by cascade rule: {0}")]
    CascadeBlocked(String),

    #[error(transparent)]
    Install(#[from] InstallError),
}
