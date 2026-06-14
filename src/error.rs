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

    #[error("`{0}` is reserved and cannot be used as a node name")]
    ReservedName(String),

    #[error("component `{0}` cannot connect to itself")]
    SelfConnection(String),

    #[error("connection `{from}` \u{2192} `{to}` already exists")]
    DuplicateConnection { from: String, to: String },

    #[error("connection `{from}` \u{2192} `{to}` does not exist")]
    ConnectionNotFound { from: String, to: String },

    #[error("{0} consistency error(s) found")]
    CheckFailed(usize),

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

    #[error("cannot determine home directory — set $HOME")]
    HomeNotFound,

    #[error("cannot determine trurlic binary path — use --binary-path")]
    BinaryNotFound,

    #[error("existing config at {path} is not valid JSON: {detail}")]
    InvalidInstallConfig { path: PathBuf, detail: String },

    #[error("existing config at {path} has unexpected structure")]
    InvalidInstallStructure { path: PathBuf },

    #[error("existing config at {path} is not valid TOML: {detail}")]
    InvalidInstallToml { path: PathBuf, detail: String },

    #[error("existing config at {path} is not valid YAML: {detail}")]
    InvalidInstallYaml { path: PathBuf, detail: String },

    #[error("`claude` CLI not found in PATH — install Claude Code first")]
    ClaudeCliNotFound,

    #[error("`claude mcp add` failed: {0}")]
    ClaudeCliExec(String),
}
