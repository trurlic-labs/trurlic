//! The failures only `trurlic install` has: locating the binary, the home
//! directory and the `claude` CLI, and reading an IDE's existing config.

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("cannot determine home directory ({0}); set $HOME")]
    HomeNotFound(std::env::VarError),

    /// The binary given or found is not a file.
    #[error("cannot determine trurlic binary path; use --binary-path")]
    BinaryNotFound,

    #[error("binary path is not valid UTF-8: {}", .0.display())]
    InvalidBinaryPath(PathBuf),

    #[error("existing config at {} is not valid JSON: {detail}", path.display())]
    InvalidJson { path: PathBuf, detail: String },

    #[error("existing config at {} is not valid TOML: {detail}", path.display())]
    InvalidToml { path: PathBuf, detail: String },

    #[error("existing config at {} is not valid YAML: {detail}", path.display())]
    InvalidYaml { path: PathBuf, detail: String },

    #[error("existing config at {} has unexpected structure: {detail}", path.display())]
    UnexpectedStructure { path: PathBuf, detail: String },

    /// The config read back from the temp file differs from what was written.
    #[error("config staged for {} did not read back as written", .0.display())]
    RoundTrip(PathBuf),

    #[error("`claude` CLI not found in PATH; install Claude Code first")]
    ClaudeCliNotFound,

    #[error("`claude mcp add` failed: {0}")]
    ClaudeCliExec(String),
}
