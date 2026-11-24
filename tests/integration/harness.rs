//! Drives the built `trurlic` binary: CLI invocations against a scratch
//! project and an MCP client speaking JSON-RPC over `trurlic serve` stdio.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Output, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
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
    /// A git repository on branch `main` whose graph its git tracks, as
    /// `init --placement tracked` makes it, committed with the
    /// `.gitignore` it writes.
    pub fn init() -> Self {
        let project = Self::without_store();
        let dir = project.path();
        git(dir, &["init", "--quiet", "--initial-branch=main"]);
        project.run_ok(&["init", "--placement", "tracked"]);
        git(dir, &["add", "--all"]);
        git(dir, &["commit", "--quiet", "--no-verify", "-m", "init"]);
        project
    }

    /// A temporary directory with no `.trurlic/`.
    pub fn without_store() -> Self {
        Self {
            dir: TempDir::new().unwrap(),
        }
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// The path of `file` under `.trurlic/`.
    pub fn node(&self, file: &str) -> PathBuf {
        self.path().join(".trurlic").join(file)
    }

    /// Write `text` to `.trurlic/<file>`, as a hand edit does.
    pub fn write(&self, file: &str, text: &str) {
        std::fs::write(self.node(file), text).unwrap();
    }

    pub fn read(&self, file: &str) -> String {
        std::fs::read_to_string(self.node(file)).unwrap()
    }

    /// A `trurlic` command with the project as its working directory: see
    /// [`trurlic`].
    pub fn command(&self) -> Command {
        trurlic(self.path())
    }

    /// Run `trurlic <args>`, assert it failed with status 1, and return its
    /// stderr.
    pub fn run_refused(&self, args: &[&str]) -> String {
        trurlic_refused(self.path(), args)
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

/// A `trurlic` command run in `dir`, with no inherited failpoint, and
/// with the git it runs isolated from the user's and the system's
/// configuration.
pub fn trurlic(dir: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_trurlic"));
    command
        .current_dir(dir)
        .env_remove("TRURLIC_FAILPOINT")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
    command
}

/// Run `trurlic <args>` in `dir`, assert it succeeded, and return its
/// stdout.
pub fn trurlic_ok(dir: &Path, args: &[&str]) -> String {
    let output = trurlic(dir).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "trurlic {args:?} in {} failed with {}\nstderr: {}",
        dir.display(),
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

/// Run `trurlic <args>` in `dir`, assert it failed with status 1, and
/// return its stderr.
pub fn trurlic_refused(dir: &Path, args: &[&str]) -> String {
    let output = trurlic(dir).args(args).output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(1),
        "trurlic {args:?} in {} must fail\nstdout: {}",
        dir.display(),
        String::from_utf8_lossy(&output.stdout)
    );
    String::from_utf8(output.stderr).unwrap()
}

/// `git <args>` in `dir`, isolated from the user's and the system's git
/// configuration, with a fixed identity; it must succeed.
pub fn git(dir: &Path, args: &[&str]) -> Output {
    let output = isolated_git(dir)
        .args([
            "-c",
            "user.name=trurlic-tests",
            "-c",
            "user.email=tests@trurlic.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

/// A `git` command in `dir` that reads neither the user's nor the system's
/// configuration and no repository the test process's environment names.
pub fn isolated_git(dir: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_WORK_TREE")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
    command
}

/// `git <args>` in the graph directory `graph`, which holds a shared
/// graph's repository: [`git`] there, with stdout as text.
pub fn graph_git(graph: &Path, args: &[&str]) -> String {
    String::from_utf8(git(graph, args).stdout).unwrap()
}

/// The commit messages of the repository of the graph directory `graph`,
/// newest first, each without its final line break.
pub fn graph_log(graph: &Path) -> Vec<String> {
    graph_git(graph, &["log", "--format=%B%x00"])
        .split('\0')
        .map(|message| message.trim_matches('\n').to_owned())
        .filter(|message| !message.is_empty())
        .collect()
}

/// A failpoint armed to pause instead of abort: the process that reaches
/// the site creates the marker file and waits there until the test deletes
/// it, holding whatever lock the site runs under.
#[cfg(feature = "failpoints")]
pub struct Pause {
    spec: &'static str,
    marker: std::path::PathBuf,
}

#[cfg(feature = "failpoints")]
impl Pause {
    /// Arm `spec` (`<site>:<n>`) for the processes given [`env`](Self::env).
    pub fn arm(project: &Project, spec: &'static str) -> Self {
        Self::arm_in(project.path(), spec)
    }

    /// [`arm`](Self::arm) with the marker file in `dir`.
    pub fn arm_in(dir: &Path, spec: &'static str) -> Self {
        Self {
            spec,
            marker: dir.join("failpoint.paused"),
        }
    }

    pub fn env(&self) -> [(&str, &str); 2] {
        [
            ("TRURLIC_FAILPOINT", self.spec),
            ("TRURLIC_FAILPOINT_PAUSE", self.marker.to_str().unwrap()),
        ]
    }

    /// Block until a process is paused at the site.
    pub fn wait(&self) {
        let deadline = std::time::Instant::now() + RESPONSE_TIMEOUT;
        while !self.marker.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "no process reached {}",
                self.spec
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    pub fn resume(&self) {
        std::fs::remove_file(&self.marker).unwrap();
    }
}

/// A decision file of `component` holding `choice` and a reason, accepted,
/// with `extra` lines appended: a hand edit `trurlic fmt` puts in order.
pub fn decision(component: &str, choice: &str, extra: &str) -> String {
    format!(
        "component = \"{component}\"\nchoice = \"{choice}\"\n\
         reason = \"Recorded so the test has a decision to read\"\n\
         status = \"accepted\"\ncreated = \"2025-06-01T10:00:00Z\"\n{extra}"
    )
}

/// The lines `stream` yields, read on a thread of their own so that every
/// receive can carry a timeout: a server that hangs or dies fails the test
/// instead of blocking it.
fn read_lines(stream: impl Read + Send + 'static) -> Receiver<String> {
    let (sender, lines) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stream).lines() {
            let Ok(line) = line else { break };
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    lines
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
        Self::spawn_with_env(project, &[])
    }

    /// Spawn `trurlic serve` with extra environment variables.
    pub fn spawn_with_env(project: &Project, vars: &[(&str, &str)]) -> Self {
        Self::spawn_in(project.path(), vars)
    }

    /// Spawn `trurlic serve` in `dir` with extra environment variables.
    pub fn spawn_in(dir: &Path, vars: &[(&str, &str)]) -> Self {
        let mut child = trurlic(dir)
            .envs(vars.iter().copied())
            .arg("serve")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take();
        let lines = read_lines(child.stdout.take().unwrap());

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
        Self::connect_with_env(project, &[])
    }

    /// [`connect`](Self::connect) with extra environment variables.
    pub fn connect_with_env(project: &Project, vars: &[(&str, &str)]) -> (Self, Value) {
        Self::connect_declaring(project, vars, json!({}))
    }

    /// [`connect`](Self::connect) with extra environment variables and the
    /// client `capabilities` the handshake declares.
    pub fn connect_declaring(
        project: &Project,
        vars: &[(&str, &str)],
        capabilities: Value,
    ) -> (Self, Value) {
        Self::connect_in(project.path(), vars, capabilities)
    }

    /// [`connect_declaring`](Self::connect_declaring) for a server started
    /// in `dir`.
    pub fn connect_in(dir: &Path, vars: &[(&str, &str)], capabilities: Value) -> (Self, Value) {
        let mut client = Self::spawn_in(dir, vars);
        let handshake = client.request(
            "initialize",
            json!({
                "protocolVersion": "2025-11-25",
                "capabilities": capabilities,
                "clientInfo": { "name": "trurlic-tests", "version": "0" }
            }),
        );
        client.notify("notifications/initialized");
        (client, handshake)
    }

    /// Write `message` as one line.
    pub fn send(&mut self, message: &Value) {
        self.send_line(&message.to_string());
    }

    /// Write `line` and a newline, whatever it holds.
    pub fn send_line(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().unwrap();
        stdin.write_all(line.as_bytes()).unwrap();
        stdin.write_all(b"\n").unwrap();
        stdin.flush().unwrap();
    }

    /// The next message the server writes.
    pub fn receive(&mut self) -> Value {
        let line = self
            .lines
            .recv_timeout(RESPONSE_TIMEOUT)
            .unwrap_or_else(|e| panic!("no message from the server: {e}"));
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("not JSON ({e}): {line}"))
    }

    /// A fresh request id, for a request sent with [`send`](Self::send).
    pub fn next_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub fn notify(&mut self, method: &str) {
        self.send(&json!({ "jsonrpc": "2.0", "method": method }));
    }

    /// Send a request and return the full JSON-RPC response.
    pub fn exchange(&mut self, method: &str, params: Value) -> Value {
        self.try_exchange(method, params)
            .unwrap_or_else(|| panic!("server closed stdout instead of answering {method}"))
    }

    /// Send a request and return the full JSON-RPC response, or `None` if
    /// the server exited without answering.
    pub fn try_exchange(&mut self, method: &str, params: Value) -> Option<Value> {
        let id = self.next_id();
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));

        let line = match self.lines.recv_timeout(RESPONSE_TIMEOUT) {
            Ok(line) => line,
            Err(RecvTimeoutError::Disconnected) => return None,
            Err(RecvTimeoutError::Timeout) => panic!("no response to {method} (id {id})"),
        };
        let response: Value = serde_json::from_str(&line)
            .unwrap_or_else(|e| panic!("response to {method} is not JSON ({e}): {line}"));
        assert_eq!(response["jsonrpc"], "2.0", "{response}");
        assert_eq!(response["id"], id, "response id mismatch: {response}");
        Some(response)
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
        let text = self.call_tool_text(name, arguments);
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name}: payload not JSON ({e})"))
    }

    /// Call a tool and return its payload as the server wrote it, failing if
    /// the tool reported `isError`.
    pub fn call_tool_text(&mut self, name: &str, arguments: Value) -> String {
        let (text, is_error) = self.call_tool_raw(name, arguments);
        assert!(!is_error, "{name} failed: {text}");
        text
    }

    /// Call a tool that must be refused and return its refusal's message.
    pub fn call_tool_error(&mut self, name: &str, arguments: Value) -> String {
        let mut refusal = self.call_tool_refusal(name, arguments);
        let Value::String(message) = refusal["message"].take() else {
            panic!("{name}: a refusal without a message: {refusal}");
        };
        message
    }

    /// Call a tool that must be refused and return its `error` object:
    /// `kind`, `message` and, for an argument, `field`.
    pub fn call_tool_refusal(&mut self, name: &str, arguments: Value) -> Value {
        let (text, is_error) = self.call_tool_raw(name, arguments);
        assert!(is_error, "{name} succeeded: {text}");
        let mut payload: Value = serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("{name}: refusal not JSON ({e}): {text}"));
        payload["error"].take()
    }

    fn call_tool_raw(&mut self, name: &str, arguments: Value) -> (String, bool) {
        let mut envelope = self.request(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        );
        let Value::String(text) = envelope["content"][0]["text"].take() else {
            panic!("{name}: no text content in {envelope}");
        };
        (text, envelope.get("isError") == Some(&Value::Bool(true)))
    }
}

impl McpClient {
    /// Close stdin and wait for the server to exit.
    #[cfg_attr(
        not(feature = "failpoints"),
        expect(dead_code, reason = "only the failpoint tests need it")
    )]
    pub fn finish(mut self) -> ExitStatus {
        drop(self.stdin.take());
        self.child.wait().unwrap()
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        // Closing stdin is the server's shutdown signal (EOF).
        drop(self.stdin.take());
        let _ = self.child.wait();
    }
}
