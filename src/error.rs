//! Typed error handling for Trurl.
//!
//! All operations return [`Result<T>`] with structured [`Error`] variants.
//! Fail-closed on writes, warn on reads.

use std::path::PathBuf;

/// Alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Every failure mode Trurl can encounter.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Command is defined but not yet implemented.
    #[error("{0}")]
    NotImplemented(String),

    /// Filesystem I/O failure.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// TOML deserialization failure.
    #[error("invalid TOML: {0}")]
    TomlRead(#[from] toml::de::Error),

    /// TOML serialization failure.
    #[error("TOML serialization error: {0}")]
    TomlWrite(#[from] toml::ser::Error),

    /// No `.trurl/` directory found in path or any parent.
    #[error("not a trurl project (no .trurl/ found in {0} or any parent directory)")]
    StoreNotFound(PathBuf),

    /// `.trurl/` already exists (e.g. double `init`).
    #[error(".trurl/ already exists at {0}")]
    StoreExists(PathBuf),

    /// Could not acquire the store lock within the timeout.
    #[error("could not acquire lock within {timeout_secs}s — {detail}")]
    LockTimeout { timeout_secs: u64, detail: String },

    /// Name failed kebab-case validation.
    #[error(
        "invalid name `{0}`: must be kebab-case (lowercase ASCII, digits, hyphens; \
             no leading/trailing/consecutive hyphens)"
    )]
    InvalidName(String),

    /// Store integrity or constraint violation.
    #[error("{0}")]
    Validation(String),
}
