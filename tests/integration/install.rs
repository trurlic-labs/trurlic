//! `trurlic install --ide claude-code` against a stand-in `claude` that logs
//! its arguments, with `PATH` and `HOME` pointing into a scratch directory.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

/// A scratch `HOME` whose `bin/` is the whole `PATH`.
struct Agent {
    home: TempDir,
}

impl Agent {
    fn new() -> Self {
        let agent = Self {
            home: TempDir::new().unwrap(),
        };
        fs::create_dir(agent.bin()).unwrap();
        agent
    }

    fn bin(&self) -> PathBuf {
        self.home.path().join("bin")
    }

    fn log(&self) -> PathBuf {
        self.home.path().join("claude.log")
    }

    /// Put a `claude` on `PATH` that appends its arguments to the log, then
    /// runs `body`.
    fn claude(&self, body: &str) {
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n{body}\n",
            self.log().display()
        );
        let claude = self.bin().join("claude");
        fs::write(&claude, script).unwrap();
        fs::set_permissions(&claude, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn install(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_trurlic"))
            .args(["install", "--ide", "claude-code"])
            .args(args)
            .env("PATH", self.bin())
            .env("HOME", self.home.path())
            .output()
            .unwrap()
    }

    fn calls(&self) -> String {
        fs::read_to_string(self.log()).unwrap_or_default()
    }
}

fn trurlic_binary() -> PathBuf {
    fs::canonicalize(Path::new(env!("CARGO_BIN_EXE_trurlic"))).unwrap()
}

#[test]
fn install_replaces_the_user_scope_entry_with_this_binary() {
    let agent = Agent::new();
    agent.claude("exit 0");

    let output = agent.install(&[]);

    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "Installed trurlic MCP server for Claude Code\n"
    );
    assert_eq!(
        agent.calls(),
        format!(
            "mcp remove trurlic -s user\nmcp add trurlic -s user -- {} serve\n",
            trurlic_binary().display()
        )
    );
}

#[test]
fn a_failed_claude_mcp_add_fails_the_command_with_its_stderr() {
    let agent = Agent::new();
    agent.claude(r#"[ "$2" = add ] && { echo 'scope refused' >&2; exit 1; }; exit 0"#);

    let output = agent.install(&[]);

    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "error: `claude mcp add` failed: scope refused\n"
    );
}

#[test]
fn a_dry_run_prints_the_command_without_claude_installed() {
    let agent = Agent::new();

    let output = agent.install(&["--dry-run"]);

    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!(
            "claude mcp add trurlic -s user -- {} serve\n",
            trurlic_binary().display()
        )
    );
}
