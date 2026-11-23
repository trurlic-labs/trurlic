//! The MCP server: one session over stdio, each request answered in turn.
//!
//! The server holds the directory it was started in and no graph. Every
//! tool call finds the store from there and loads it, so a call answers
//! from the files on disk, and a store created or migrated while the
//! server runs is served from the next call. Without one, `initialize` and
//! `tools/list` still answer, and every tool call is refused naming the
//! command that fixes it.

mod context;
mod pattern;
#[cfg_attr(
    not(any(test, feature = "failpoints")),
    expect(
        dead_code,
        reason = "no tool sends the client a request until `accept` asks through an \
                  elicitation form"
    )
)]
mod peer;
mod protocol;
mod result;
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "no tool is built on a spec until the first 0.4.0 tool lands"
    )
)]
mod spec;
mod tools;
mod truncate;
mod update;
mod verify;
mod write;

use std::any::Any;
use std::io::{self, BufRead, Write};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::Result;
use crate::console::diag;
use crate::store::Store;
#[cfg(feature = "failpoints")]
use crate::store::failpoint::{self, Site};

use peer::{Client, ClientCapabilities, Peer};
use protocol::{
    INTERNAL_ERROR, INVALID_PARAMS, INVALID_REQUEST, METHOD_NOT_FOUND, Message, Request,
};
use result::{ToolEnvelope, tool_error, tool_result};

/// The protocol revisions the server speaks. `initialize` answers with the
/// client's when it is one of them, else with the latest.
const PROTOCOL_VERSIONS: [&str; 2] = [LATEST_PROTOCOL_VERSION, "2025-06-18"];
const LATEST_PROTOCOL_VERSION: &str = "2025-11-25";

// -- Public entry point ----------------------------------------------------

/// Run the MCP server on stdio for the store found from `cwd` on each call.
pub(crate) fn run_server(cwd: PathBuf) -> Result<()> {
    let mut server = Server::new(cwd);

    let mut reader = io::stdin().lock();
    #[expect(
        clippy::disallowed_methods,
        reason = "the JSON-RPC transport owns stdout"
    )]
    let mut writer = io::stdout().lock();

    diag!("trurlic: MCP server ready");

    loop {
        match server.serve_one(&mut reader, &mut writer) {
            Ok(true) => {}
            Ok(false) => break,
            Err(e) => {
                diag!("trurlic: stdio failed: {e}");
                break;
            }
        }
    }

    diag!("trurlic: MCP server stopped");
    Ok(())
}

/// One MCP session: the directory its tool calls find the store from, and
/// what it keeps of the client.
pub struct Server {
    cwd: PathBuf,
    client: Client,
}

impl Server {
    #[must_use]
    pub fn new(cwd: PathBuf) -> Self {
        Self {
            cwd,
            client: Client::default(),
        }
    }

    /// Answer the next request, a queued one first, else the next line of
    /// `reader`, writing its answer, if it has one, to `writer`.
    /// `Ok(false)` at the end of the input; an error is a failed read or
    /// write of the stream, after which the session cannot answer.
    pub fn serve_one(
        &mut self,
        reader: &mut impl BufRead,
        writer: &mut impl Write,
    ) -> io::Result<bool> {
        let message = match self.client.next_queued() {
            Some(request) => Ok(Message::Request(request)),
            None => match protocol::read_message(reader)? {
                Some(message) => message,
                None => return Ok(false),
            },
        };
        match message {
            Ok(Message::Request(request)) => self.answer(&request, reader, writer)?,
            // `notifications/initialized` needs no action, and a
            // cancellation read here names a request already answered.
            Ok(Message::Notification(_)) => {}
            Ok(Message::Response(response)) => diag!(
                "trurlic: ignoring a response to request {}, which nothing waits for",
                response.id
            ),
            Err(malformed) => {
                protocol::write_error(writer, &malformed.id, malformed.code, &malformed.message)?;
            }
        }
        Ok(true)
    }

    fn answer(
        &mut self,
        request: &Request,
        reader: &mut dyn BufRead,
        writer: &mut dyn Write,
    ) -> io::Result<()> {
        // The unwind boundary: a panic while a request is handled answers
        // that request -32603 and the server serves the next. Nothing the
        // panic interrupts outlives it half-done. The server holds no
        // graph; a writer's file lock is released as unwinding drops it,
        // and a commit it began is finished from the journal by the next
        // opener, as after a crash. `self.client` changes only by whole
        // steps (an id taken, a request queued or dequeued), and every line
        // sent to the client is built in memory before it is written, so
        // the stream never holds part of one.
        let reply = panic::catch_unwind(AssertUnwindSafe(|| {
            self.reply(request, &mut *reader, &mut *writer)
        }))
        .unwrap_or_else(|payload| Reply::Failure {
            code: INTERNAL_ERROR,
            message: panicked(&request.method, &*payload),
        });
        match reply {
            Reply::Success(body) => protocol::write_success(writer, &request.id, &body),
            Reply::Failure { code, message } => {
                protocol::write_error(writer, &request.id, code, &message)
            }
            Reply::Cancelled => Ok(()),
        }
    }

    fn reply(
        &mut self,
        request: &Request,
        reader: &mut dyn BufRead,
        writer: &mut dyn Write,
    ) -> Reply {
        let params = request.params.as_ref();
        match request.method.as_str() {
            "initialize" => self.initialize(params),
            "ping" => Reply::Success(Body::Value(json!({}))),
            "tools/list" | "tools/call" if !self.client.is_initialized() => Reply::Failure {
                code: INVALID_REQUEST,
                message: "server not initialized: send initialize first".into(),
            },
            "tools/list" => Reply::Success(Body::Listed(tools::tool_list())),
            "tools/call" => {
                let mut peer = Peer::new(reader, writer, &mut self.client, &request.id);
                let reply = call_tool(&self.cwd, params, &mut peer);
                if peer.cancelled() {
                    Reply::Cancelled
                } else {
                    reply
                }
            }
            method => Reply::Failure {
                code: METHOD_NOT_FOUND,
                message: format!("unknown method: {method}"),
            },
        }
    }

    fn initialize(&mut self, params: Option<&Map<String, Value>>) -> Reply {
        let Some(params) = params else {
            return invalid_params("initialize needs params");
        };
        let Some(requested) = params.get("protocolVersion").and_then(Value::as_str) else {
            return invalid_params("initialize needs a string `protocolVersion`");
        };
        self.client
            .initialize(ClientCapabilities::declared_in(params));
        Reply::Success(Body::Value(json!({
            "protocolVersion": negotiate(requested),
            "capabilities": { "tools": {} },
            "serverInfo": {
                "name": "trurlic",
                "version": env!("CARGO_PKG_VERSION"),
            },
        })))
    }
}

/// The client's revision when the server speaks it, else the latest.
fn negotiate(requested: &str) -> &'static str {
    PROTOCOL_VERSIONS
        .into_iter()
        .find(|version| *version == requested)
        .unwrap_or(LATEST_PROTOCOL_VERSION)
}

/// What a request is answered with, written once its handling is over.
enum Reply {
    Success(Body),
    Failure {
        code: i32,
        message: String,
    },
    /// The client cancelled the request, so it gets no response.
    Cancelled,
}

/// A `result`, serialized directly into the response.
#[derive(Serialize)]
#[serde(untagged)]
enum Body {
    Value(Value),
    Listed(&'static Value),
    Tool(ToolEnvelope),
}

fn invalid_params(message: impl Into<String>) -> Reply {
    Reply::Failure {
        code: INVALID_PARAMS,
        message: message.into(),
    }
}

fn panicked(method: &str, payload: &(dyn Any + Send)) -> String {
    let detail = payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("no message");
    format!("internal error: `{method}` panicked: {detail}")
}

// -- Tool calls ------------------------------------------------------------

static NO_ARGUMENTS: LazyLock<Value> = LazyLock::new(|| json!({}));

/// A tool listed in `tools/list`, called on the store found from `cwd`. A
/// malformed call or an unknown tool is a protocol error; anything the
/// tool refuses, a missing store included, is a tool result with
/// `isError`.
fn call_tool(cwd: &Path, params: Option<&Map<String, Value>>, peer: &mut Peer<'_>) -> Reply {
    let Some(params) = params else {
        return invalid_params("tools/call needs params");
    };
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return invalid_params("tools/call needs a string `name`");
    };
    let arguments = params.get("arguments").unwrap_or(&NO_ARGUMENTS);
    let Some(object) = arguments.as_object() else {
        return invalid_params("`arguments` must be an object");
    };
    let spec_tool = spec::find(name);
    if spec_tool.is_none() && !tools::is_listed(name) {
        return invalid_params(format!("unknown tool: {name}"));
    }
    #[cfg(feature = "failpoints")]
    if failpoint::triggers(Site::ServerRequest) {
        return Reply::Success(Body::Tool(ask_the_client(peer)));
    }
    let envelope = match Store::discover(cwd) {
        Err(e) => tool_error(&e.into()),
        Ok(store) => match spec_tool {
            Some(tool) => tool.call(&store, peer, object).map_or_else(
                |refused| tool_error(&refused),
                |payload| tool_result(&payload),
            ),
            None => call_unspecified(&store, name, arguments),
        },
    };
    Reply::Success(Body::Tool(envelope))
}

/// A tool that predates specs: a write tool runs on a
/// [`Writer`](crate::store::Writer), the graph loaded under the exclusive
/// lock, so it validates against every commit another process made; a
/// read tool on the graph loaded under the shared lock. Either load fails
/// as a refusal.
fn call_unspecified(store: &Store, name: &str, arguments: &Value) -> ToolEnvelope {
    let envelope = if tools::is_write_tool(name) {
        store
            .begin_write()
            .map(|mut writer| tools::call_write_tool(&mut writer, name, arguments))
    } else {
        store
            .load_state()
            .map(|state| tools::call_read_tool(&state, name, arguments))
    };
    envelope.unwrap_or_else(|e| tool_error(&e.into()))
}

/// The armed tool call sends the client a form in its place, and answers
/// with the client's answer.
#[cfg(feature = "failpoints")]
fn ask_the_client(peer: &mut Peer<'_>) -> ToolEnvelope {
    let form = json!({
        "message": "Confirm the failpoint",
        "requestedSchema": {
            "type": "object",
            "properties": { "confirmed": { "type": "boolean" } },
            "required": ["confirmed"],
        },
    });
    match peer.request(peer::ServerMethod::CreateElicitation, &form) {
        Ok(answer) => tool_result(&answer),
        Err(e) => tool_error(&result::ToolError::refused(e.to_string())),
    }
}

// -- Tests ----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn initialize(version: &str) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "initialize",
            "params": { "protocolVersion": version, "capabilities": {} },
        })
    }

    fn request(id: u64, method: &str, params: Value) -> Value {
        json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
    }

    fn tool_call(id: u64, name: &str, arguments: Value) -> Value {
        request(
            id,
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        )
    }

    /// Serve `messages` to the end and return every response, in order.
    fn serve(server: &mut Server, messages: &[Value]) -> Vec<Value> {
        let input: Vec<u8> = messages
            .iter()
            .flat_map(|message| format!("{message}\n").into_bytes())
            .collect();
        let mut reader = &input[..];
        let mut output = Vec::new();
        while server.serve_one(&mut reader, &mut output).unwrap() {}
        let text = String::from_utf8(output).unwrap();
        text.lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn project() -> (tempfile::TempDir, Server) {
        let tmp = tempfile::TempDir::new().unwrap();
        crate::commands::init(tmp.path()).unwrap();
        let server = Server::new(tmp.path().to_path_buf());
        (tmp, server)
    }

    fn initialized() -> (tempfile::TempDir, Server) {
        let (tmp, mut server) = project();
        serve(&mut server, &[initialize(LATEST_PROTOCOL_VERSION)]);
        (tmp, server)
    }

    #[test]
    fn initialize_answers_a_supported_version_with_itself() {
        for version in PROTOCOL_VERSIONS {
            let (_tmp, mut server) = project();
            let responses = serve(&mut server, &[initialize(version)]);
            assert_eq!(responses[0]["result"]["protocolVersion"], version);
            assert!(responses[0]["result"]["capabilities"]["tools"].is_object());
        }
    }

    #[test]
    fn initialize_answers_any_other_version_with_the_latest() {
        for version in ["2024-11-05", "2025-03-26", "1.0", ""] {
            let (_tmp, mut server) = project();
            let responses = serve(&mut server, &[initialize(version)]);
            assert_eq!(
                responses[0]["result"]["protocolVersion"], LATEST_PROTOCOL_VERSION,
                "{version}"
            );
        }
    }

    #[test]
    fn initialize_without_a_version_is_invalid_params() {
        let (_tmp, mut server) = project();
        let responses = serve(
            &mut server,
            &[
                request(1, "initialize", json!({ "capabilities": {} })),
                json!({ "jsonrpc": "2.0", "id": 2, "method": "initialize" }),
                request(3, "tools/list", json!({})),
            ],
        );
        assert_eq!(responses[0]["error"]["code"], INVALID_PARAMS);
        assert_eq!(responses[1]["error"]["code"], INVALID_PARAMS);
        assert_eq!(
            responses[2]["error"]["code"], INVALID_REQUEST,
            "a refused initialize leaves the session uninitialized"
        );
    }

    #[test]
    fn tools_are_refused_before_initialize_and_ping_is_not() {
        let (_tmp, mut server) = project();
        let responses = serve(
            &mut server,
            &[
                request(1, "tools/list", json!({})),
                tool_call(2, "get_architecture", json!({})),
                request(3, "ping", json!({})),
            ],
        );
        assert_eq!(responses[0]["error"]["code"], INVALID_REQUEST);
        assert_eq!(responses[1]["error"]["code"], INVALID_REQUEST);
        assert_eq!(responses[2]["result"], json!({}));
    }

    #[test]
    fn notifications_and_stray_responses_are_not_answered() {
        let (_tmp, mut server) = initialized();
        let responses = serve(
            &mut server,
            &[
                json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
                json!({ "jsonrpc": "2.0", "id": 5, "result": {} }),
                request(6, "ping", json!({})),
            ],
        );
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0]["id"], 6);
    }

    #[test]
    fn protocol_errors_carry_their_codes() {
        let (_tmp, mut server) = initialized();
        let responses = serve(
            &mut server,
            &[
                request(1, "bogus/method", json!({})),
                json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call" }),
                request(3, "tools/call", json!({ "arguments": {} })),
                request(
                    4,
                    "tools/call",
                    json!({ "name": "get_architecture", "arguments": [] }),
                ),
                tool_call(5, "nonexistent", json!({})),
            ],
        );
        let codes: Vec<&Value> = responses.iter().map(|r| &r["error"]["code"]).collect();
        assert_eq!(
            codes,
            [
                &json!(METHOD_NOT_FOUND),
                &json!(INVALID_PARAMS),
                &json!(INVALID_PARAMS),
                &json!(INVALID_PARAMS),
                &json!(INVALID_PARAMS),
            ]
        );
        assert!(
            responses[4]["error"]["message"]
                .as_str()
                .unwrap()
                .contains("nonexistent")
        );
    }

    fn component_names(response: &Value) -> Vec<String> {
        let text = response["result"]["content"][0]["text"].as_str().unwrap();
        let payload: Value = serde_json::from_str(text).unwrap();
        payload["components"]
            .as_array()
            .unwrap()
            .iter()
            .map(|component| component["name"].as_str().unwrap().to_owned())
            .collect()
    }

    /// No state survives a call: the next read loads what another process
    /// committed after the previous one.
    #[test]
    fn a_read_tool_serves_a_commit_made_since_the_last_call() {
        let (tmp, mut server) = initialized();
        let first = serve(&mut server, &[tool_call(1, "get_architecture", json!({}))]);
        assert!(component_names(&first[0]).is_empty(), "{}", first[0]);

        let store = Store::discover(tmp.path()).unwrap();
        store
            .begin_write()
            .unwrap()
            .add_component("auth", "")
            .unwrap();

        let second = serve(&mut server, &[tool_call(2, "get_architecture", json!({}))]);
        assert_eq!(component_names(&second[0]), ["auth"]);
    }

    /// The lost update: another process commits after this server's last
    /// call. The write must validate against that commit and keep it.
    #[test]
    fn write_tool_validates_against_commits_made_since_the_last_call() {
        let (tmp, mut server) = initialized();
        serve(&mut server, &[tool_call(1, "get_architecture", json!({}))]);
        let store = Store::discover(tmp.path()).unwrap();
        let mut other = store.begin_write().unwrap();
        other.add_component("auth", "").unwrap();
        other.add_component("billing", "").unwrap();
        drop(other);

        let responses = serve(
            &mut server,
            &[tool_call(
                2,
                "add_connection",
                json!({ "from": "auth", "to": "billing" }),
            )],
        );

        assert_ne!(
            responses[0]["result"]["isError"],
            json!(true),
            "{}",
            responses[0]
        );
        let on_disk = store.load_state().unwrap();
        assert!(on_disk.components.contains_key("auth"));
        assert!(on_disk.components.contains_key("billing"));
        assert_eq!(on_disk.graph().connects_to("auth"), ["billing"]);
    }

    /// A store is found on every call: a refusal naming `trurlic init`
    /// before it exists, an answer once it does.
    #[test]
    fn a_tool_call_finds_the_store_made_after_the_server_started() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut server = Server::new(tmp.path().to_path_buf());
        let before = serve(
            &mut server,
            &[
                initialize(LATEST_PROTOCOL_VERSION),
                tool_call(1, "get_architecture", json!({})),
            ],
        );
        let text = before[1]["result"]["content"][0]["text"].as_str().unwrap();
        let refusal: Value = serde_json::from_str(text).unwrap();
        assert_eq!(before[1]["result"]["isError"], true);
        assert_eq!(refusal["error"]["kind"], "no_project");
        assert!(
            refusal["error"]["message"]
                .as_str()
                .unwrap()
                .contains("trurlic init")
        );

        crate::commands::init(tmp.path()).unwrap();
        let after = serve(&mut server, &[tool_call(2, "get_architecture", json!({}))]);
        assert_eq!(component_names(&after[0]), Vec::<String>::new());
    }

    #[test]
    fn a_tool_result_carries_its_payload_as_json_text() {
        let (_tmp, mut server) = initialized();
        let responses = serve(&mut server, &[tool_call(1, "get_architecture", json!({}))]);
        let text = responses[0]["result"]["content"][0]["text"]
            .as_str()
            .unwrap();
        let payload: Value = serde_json::from_str(text).unwrap();
        assert!(payload.is_object(), "{payload}");
    }

    #[test]
    fn a_panic_message_names_the_method_and_the_payload() {
        let message = panicked("tools/call", &"boom");
        assert_eq!(message, "internal error: `tools/call` panicked: boom");
        let message = panicked("ping", &String::from("owned"));
        assert!(message.ends_with("owned"), "{message}");
        let message = panicked("ping", &5_u8);
        assert!(message.ends_with("no message"), "{message}");
    }
}
