//! Drives the built `trurlic` binary: CLI invocations against a scratch
//! project, and an MCP client speaking JSON-RPC over `trurlic serve` stdio.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::TempDir;

/// How long one response may take before the test fails instead of hanging.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

/// A `trurlic init`-ed project in a temporary directory.
pub struct Project {
    dir: TempDir,
}

impl Project {
    pub fn init() -> Self {
        let project = Self {
            dir: TempDir::new().unwrap(),
        };
        project.run_ok(&["init"]);
        project
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// A `trurlic` command with the project as its working directory and no
    /// inherited failpoint.
    pub fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_trurlic"));
        command
            .current_dir(self.path())
            .env_remove("TRURLIC_FAILPOINT");
        command
    }

    /// Run `trurlic <args>` and assert it succeeded.
    pub fn run_ok(&self, args: &[&str]) -> Output {
        let output = self.command().args(args).output().unwrap();
        assert!(
            output.status.success(),
            "trurlic {args:?} failed with {}\nstderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }
}

/// An MCP client connected to `trurlic serve` over stdio.
pub struct McpClient {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    next_id: u64,
}

impl McpClient {
    /// Spawn `trurlic serve` in `project`, without initializing the session.
    pub fn spawn(project: &Project) -> Self {
        let mut child = project
            .command()
            .arg("serve")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().unwrap();

        // A reader thread lets every receive carry a timeout, so a server
        // that hangs or dies fails the test instead of blocking it.
        let (sender, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if sender.send(line).is_err() {
                    break;
                }
            }
        });

        Self {
            child,
            stdin,
            lines,
            next_id: 1,
        }
    }

    /// Spawn and complete the `initialize` handshake. Returns the
    /// `initialize` result.
    pub fn connect(project: &Project) -> (Self, Value) {
        let mut client = Self::spawn(project);
        let result = client.request(
            "initialize",
            json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "trurlic-tests", "version": "0" }
            }),
        );
        client.notify("notifications/initialized");
        (client, result)
    }

    fn send(&mut self, message: &Value) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{message}").unwrap();
        stdin.flush().unwrap();
    }

    pub fn notify(&mut self, method: &str) {
        self.send(&json!({ "jsonrpc": "2.0", "method": method }));
    }

    /// Send a request and return the full JSON-RPC response.
    pub fn exchange(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));

        let line = self
            .lines
            .recv_timeout(RESPONSE_TIMEOUT)
            .unwrap_or_else(|e| panic!("no response to {method} (id {id}): {e}"));
        let response: Value = serde_json::from_str(&line)
            .unwrap_or_else(|e| panic!("response to {method} is not JSON ({e}): {line}"));
        assert_eq!(response["jsonrpc"], "2.0", "{response}");
        assert_eq!(response["id"], id, "response id mismatch: {response}");
        response
    }

    /// Send a request and return its `result`, failing on a JSON-RPC error.
    pub fn request(&mut self, method: &str, params: Value) -> Value {
        let mut response = self.exchange(method, params);
        assert!(
            response.get("error").is_none(),
            "{method} returned an error: {response}"
        );
        response["result"].take()
    }

    /// Call a tool and return its decoded payload, failing if the tool
    /// reported `isError`.
    pub fn call_tool(&mut self, name: &str, arguments: Value) -> Value {
        let result = self.request(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        );
        let text = result["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("{name}: no text content in {result}"));
        assert_ne!(
            result.get("isError"),
            Some(&Value::Bool(true)),
            "{name} failed: {text}"
        );
        serde_json::from_str(text).unwrap_or_else(|e| panic!("{name}: payload not JSON ({e})"))
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        // Closing stdin is the server's shutdown signal (EOF).
        drop(self.stdin.take());
        let _ = self.child.wait();
    }
}
