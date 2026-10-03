//! Drives the built `trurlic` binary: CLI invocations against a scratch
//! project, an MCP client speaking JSON-RPC over `trurlic serve` stdio, and
//! an HTTP client for the REST API of `trurlic map`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
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
        Self {
            spec,
            marker: project.path().join("failpoint.paused"),
        }
    }

    pub fn env(&self) -> [(&str, &str); 2] {
        [
            ("TRURLIC_FAILPOINT", self.spec),
            ("TRURLIC_FAILPOINT_PAUSE", self.marker.to_str().unwrap()),
        ]
    }

    pub fn marker(&self) -> &Path {
        &self.marker
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

/// Wait for the first line in `lines` that contains `needle`.
fn wait_for_line(lines: &Receiver<String>, needle: &str) -> String {
    loop {
        match lines.recv_timeout(RESPONSE_TIMEOUT) {
            Ok(line) if line.contains(needle) => return line,
            Ok(_) => {}
            Err(e) => panic!("no line containing {needle:?}: {e}"),
        }
    }
}

/// An MCP client connected to `trurlic serve` over stdio.
pub struct McpClient {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    diagnostics: Receiver<String>,
    next_id: u64,
}

impl McpClient {
    /// Spawn `trurlic serve` in `project`, without initializing the session.
    pub fn spawn(project: &Project) -> Self {
        Self::spawn_with_env(project, &[])
    }

    /// Spawn `trurlic serve` with extra environment variables.
    pub fn spawn_with_env(project: &Project, vars: &[(&str, &str)]) -> Self {
        let mut child = project
            .command()
            .envs(vars.iter().copied())
            .arg("serve")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take();
        let lines = read_lines(child.stdout.take().unwrap());
        let diagnostics = read_lines(child.stderr.take().unwrap());

        Self {
            child,
            stdin,
            lines,
            diagnostics,
            next_id: 1,
        }
    }

    /// Wait for the first line the server writes to stderr that contains
    /// `needle`.
    pub fn wait_for_diagnostic(&self, needle: &str) -> String {
        wait_for_line(&self.diagnostics, needle)
    }

    /// Spawn and complete the `initialize` handshake. Returns the
    /// `initialize` result.
    pub fn connect(project: &Project) -> (Self, Value) {
        Self::connect_with_env(project, &[])
    }

    /// [`connect`](Self::connect) with extra environment variables.
    pub fn connect_with_env(project: &Project, vars: &[(&str, &str)]) -> (Self, Value) {
        let mut client = Self::spawn_with_env(project, vars);
        let handshake = client.request(
            "initialize",
            json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "trurlic-tests", "version": "0" }
            }),
        );
        client.notify("notifications/initialized");
        (client, handshake)
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
        self.try_exchange(method, params)
            .unwrap_or_else(|| panic!("server closed stdout instead of answering {method}"))
    }

    /// Send a request and return the full JSON-RPC response, or `None` if
    /// the server exited without answering.
    pub fn try_exchange(&mut self, method: &str, params: Value) -> Option<Value> {
        let id = self.next_id;
        self.next_id += 1;
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

    /// Call a tool that must fail and return its error message.
    pub fn call_tool_error(&mut self, name: &str, arguments: Value) -> String {
        let (text, is_error) = self.call_tool_raw(name, arguments);
        assert!(is_error, "{name} succeeded: {text}");
        text
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

/// A `trurlic map` server and an HTTP/1.1 client for its REST API.
pub struct MapServer {
    child: Child,
    /// `host:port` the server listens on.
    address: String,
    token: String,
}

impl MapServer {
    /// Start `trurlic map` in `project` and wait until its file watcher runs.
    pub fn start(project: &Project) -> Self {
        Self::start_with_env(project, &[])
    }

    /// [`start`](Self::start) with extra environment variables.
    pub fn start_with_env(project: &Project, vars: &[(&str, &str)]) -> Self {
        let mut child = project
            .command()
            .envs(vars.iter().copied())
            .args(["map", "--no-open"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();

        // The server prints its URL, then starts the watcher.
        let lines = read_lines(child.stderr.take().unwrap());

        let mut url = None;
        loop {
            let line = lines
                .recv_timeout(RESPONSE_TIMEOUT)
                .expect("map server exited or stalled before its watcher started");
            if let Some((_, found)) = line.split_once("http://") {
                url = Some(found.to_owned());
            }
            if line.contains("file watcher active") {
                break;
            }
        }
        let url = url.expect("map server printed no URL");
        let (address, token) = url.split_once("/?token=").unwrap();

        Self {
            child,
            address: address.to_owned(),
            token: token.to_owned(),
        }
    }

    /// Send a request to the REST API and return the status code and the
    /// decoded JSON body.
    pub fn request(&self, method: &str, path: &str, body: Option<&Value>) -> (u16, Value) {
        let body = body.map(Value::to_string).unwrap_or_default();
        let mut stream = TcpStream::connect(&self.address).unwrap();
        stream.set_read_timeout(Some(RESPONSE_TIMEOUT)).unwrap();
        write!(
            stream,
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\n\
             Content-Type: application/json\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{body}",
            self.address,
            self.token,
            body.len()
        )
        .unwrap();

        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let (head, payload) = response
            .split_once("\r\n\r\n")
            .unwrap_or_else(|| panic!("malformed HTTP response: {response}"));
        let status = head
            .split(' ')
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or_else(|| panic!("no status in {head}"));
        let json = serde_json::from_str(payload)
            .unwrap_or_else(|e| panic!("{method} {path}: body is not JSON ({e}): {payload}"));
        (status, json)
    }
}

impl Drop for MapServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
