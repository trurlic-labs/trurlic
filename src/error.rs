use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("invalid TOML: {0}")]
    TomlRead(#[from] toml::de::Error),

    #[error("TOML serialization error: {0}")]
    TomlWrite(#[from] toml::ser::Error),

    #[error("not a trurl project (no .trurl/ found in {0} or any parent directory)")]
    StoreNotFound(PathBuf),

    #[error(".trurl/ already exists at {0}")]
    StoreExists(PathBuf),

    #[error("could not acquire lock within {timeout_secs}s — {detail}")]
    LockTimeout { timeout_secs: u64, detail: String },

    #[error(
        "invalid name `{0}`: must be kebab-case (lowercase ASCII, digits, hyphens; \
             no leading/trailing/consecutive hyphens)"
    )]
    InvalidName(String),

    #[error("{0}")]
    Validation(String),

    #[error("graph integrity violation: {0}")]
    GraphIntegrity(String),

    #[error("operation blocked by cascade rule: {0}")]
    CascadeBlocked(String),

    #[error("{0}")]
    ProviderConfig(String),

    /// `status` is the HTTP status code, or `0` for connection-level
    /// failures (timeout, DNS, TLS, stream stall).
    #[error("API error ({status}): {detail}")]
    Api { status: u16, detail: String },
}
