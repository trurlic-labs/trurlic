//! `trurlic install`: registers the trurlic MCP server with Claude Code at
//! user scope by running `claude mcp add`, so Claude Code reads and writes
//! its own config. The command never touches a `.trurlic/` store.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::Error;
use crate::console::out;

use super::DryRun;

/// A failure of `trurlic install`. It is not a variant of [`crate::Error`];
/// [`Io`](Self::Io) carries the failures the two share.
#[derive(Debug, thiserror::Error)]
pub(crate) enum InstallError {
    #[error("cannot determine home directory ({0}); set $HOME")]
    HomeNotFound(std::env::VarError),

    /// The binary given or found is not a file.
    #[error("cannot determine trurlic binary path; use --binary-path")]
    BinaryNotFound,

    #[error("binary path is not valid UTF-8: {}", .0.display())]
    InvalidBinaryPath(PathBuf),

    #[error("`claude` CLI not found in PATH; install Claude Code first")]
    ClaudeCliNotFound,

    /// `claude mcp add` exited unsuccessfully; the field is its trimmed stderr.
    #[error("`claude mcp add` failed: {0}")]
    ClaudeMcpAddFailed(String),

    /// Locating the binary, starting `claude` or writing to stdout failed;
    /// the error names the file or the call.
    #[error(transparent)]
    Io(#[from] Error),
}

/// Register `binary serve` as the `trurlic` MCP server of Claude Code, or
/// with `DryRun::Yes` print the `claude` command that would, without
/// needing Claude Code installed.
pub(crate) fn install(binary_path: Option<&Path>, dry_run: DryRun) -> Result<(), InstallError> {
    let resolved = resolve_binary(binary_path)?;
    let Some(binary) = resolved.to_str() else {
        return Err(InstallError::InvalidBinaryPath(resolved));
    };

    if dry_run == DryRun::Yes {
        out!("claude mcp add trurlic -s user -- {binary} serve")?;
        return Ok(());
    }

    let claude = find_claude_cli()?;

    // Best-effort removal of a stale entry: the add below reports any real
    // failure. Closing stdin keeps a prompting subprocess from hanging.
    let _ = Command::new(&claude)
        .args(["mcp", "remove", "trurlic", "-s", "user"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .output();

    let output = Command::new(&claude)
        .args(["mcp", "add", "trurlic", "-s", "user", "--", binary, "serve"])
        .output()
        .map_err(Error::io(&claude))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(InstallError::ClaudeMcpAddFailed(stderr.trim().to_owned()));
    }

    out!("Installed trurlic MCP server for Claude Code")?;
    Ok(())
}

/// The canonical path of `explicit`, or of the running binary without it.
fn resolve_binary(explicit: Option<&Path>) -> Result<PathBuf, InstallError> {
    let path = match explicit {
        Some(path) => path.to_path_buf(),
        None => std::env::current_exe().map_err(Error::system("locate the trurlic binary"))?,
    };
    if !path.is_file() {
        return Err(InstallError::BinaryNotFound);
    }
    Ok(fs::canonicalize(&path).map_err(Error::io(&path))?)
}

fn find_claude_cli() -> Result<PathBuf, InstallError> {
    if let Some(claude) = which("claude") {
        return Ok(claude);
    }
    let home = home_dir()?;
    [
        PathBuf::from("/usr/local/bin/claude"),
        home.join(".claude/bin/claude"),
    ]
    .into_iter()
    .find(|candidate| candidate.is_file())
    .ok_or(InstallError::ClaudeCliNotFound)
}

/// The first file named `name` in a `PATH` directory; on Windows also
/// `name.exe` and `name.cmd`.
fn which(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var).find_map(|dir| {
        let candidates = [
            dir.join(name),
            #[cfg(windows)]
            dir.join(format!("{name}.exe")),
            #[cfg(windows)]
            dir.join(format!("{name}.cmd")),
        ];
        candidates.into_iter().find(|candidate| candidate.is_file())
    })
}

fn home_dir() -> Result<PathBuf, InstallError> {
    #[cfg(windows)]
    let home = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME"));
    #[cfg(not(windows))]
    let home = std::env::var("HOME");
    home.map(PathBuf::from).map_err(InstallError::HomeNotFound)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn a_missing_binary_is_refused() {
        let err = resolve_binary(Some(Path::new("/nonexistent/trurlic"))).unwrap_err();
        assert!(matches!(err, InstallError::BinaryNotFound), "{err:?}");
    }

    #[test]
    fn the_binary_is_resolved_to_its_canonical_path() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("trurlic"), "fake binary").unwrap();
        let given = tmp.path().join(".").join("trurlic");

        let resolved = resolve_binary(Some(&given)).unwrap();

        assert_eq!(
            resolved,
            fs::canonicalize(tmp.path()).unwrap().join("trurlic")
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_binary_path_that_is_not_utf8_is_refused() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let tmp = TempDir::new().unwrap();
        let binary = tmp.path().join(OsStr::from_bytes(b"trurlic-\xff"));
        fs::write(&binary, "fake binary").unwrap();

        let err = install(Some(&binary), DryRun::Yes).unwrap_err();
        assert!(matches!(err, InstallError::InvalidBinaryPath(_)), "{err:?}");
    }
}
