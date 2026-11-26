//! The MCP server: one session over stdio, each request answered in turn.
//!
//! The server holds the directory it was started in and no graph. Every
//! tool call hands that directory to the tool, which finds the store from
//! there and loads it, so a call answers from the files on disk, and a
//! store created or migrated while the server runs is served from the next
//! call. Without one, `initialize` and `tools/list` still answer, and
//! every tool call is refused naming the command that fixes it.

mod envelope;
mod listing;
mod peer;
mod protocol;

use std::any::Any;
use std::io::{self, BufRead, Write};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::Result;
use crate::console::diag;
#[cfg(feature = "failpoints")]
use crate::store::failpoint::{self, Site};
#[cfg(feature = "failpoints")]
use crate::tools::ToolError;
use crate::tools::{Tool, ToolResult};

use envelope::ToolEnvelope;
use peer::{Client, ClientCapabilities, Peer};
use protocol::{
    INTERNAL_ERROR, INVALID_PARAMS, INVALID_REQUEST, METHOD_NOT_FOUND, Message, Request,
};

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
            "tools/list" => Reply::Success(Body::Listed(listing::tool_list())),
            "tools/call" => {
                let mut peer = Peer::new(reader, writer, &mut self.client, &request.id);
                call_tool(&self.cwd, params, &mut peer)
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
/// `isError`; a call the client cancelled while it waited gets no answer.
fn call_tool(cwd: &Path, params: Option<&Map<String, Value>>, peer: &mut Peer<'_>) -> Reply {
    let Some(params) = params else {
        return invalid_params("tools/call needs params");
    };
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return invalid_params("tools/call needs a string `name`");
    };
    let arguments = params.get("arguments").unwrap_or(&NO_ARGUMENTS);
    let Some(arguments) = arguments.as_object() else {
        return invalid_params("`arguments` must be an object");
    };
    let Some(tool) = Tool::named(name) else {
        return invalid_params(format!("unknown tool: {name}"));
    };
    #[cfg(feature = "failpoints")]
    if failpoint::triggers(Site::ServerRequest) {
        let result = ask_the_client(peer);
        return answered(peer, result);
    }
    let result = tool.call(cwd, arguments);
    answered(peer, result)
}

/// `result` as the call's reply, or none when the client cancelled the
/// call while it waited.
fn answered(peer: &Peer<'_>, result: ToolResult) -> Reply {
    if peer.cancelled() {
        Reply::Cancelled
    } else {
        Reply::Success(Body::Tool(result.into()))
    }
}

/// The armed tool call sends the client a form in its place, and answers
/// with the client's answer.
#[cfg(feature = "failpoints")]
fn ask_the_client(peer: &mut Peer<'_>) -> ToolResult {
    let form = json!({
        "message": "Confirm the failpoint",
        "requestedSchema": {
            "type": "object",
            "properties": { "confirmed": { "type": "boolean" } },
            "required": ["confirmed"],
        },
    });
    match peer.request(peer::ServerMethod::CreateElicitation, &form) {
        Ok(answer) => ToolResult::unstructured(&answer),
        Err(e) => ToolResult::refusal(&ToolError::refused(e.to_string())),
    }
}

// -- Tests ----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Placement, Store};

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
        Store::create(tmp.path(), "demo", Placement::Tracked).unwrap();
        let server = Server::new(tmp.path().to_path_buf());
        (tmp, server)
    }

    fn initialized() -> (tempfile::TempDir, Server) {
        let (tmp, mut server) = project();
        serve(&mut server, &[initialize(LATEST_PROTOCOL_VERSION)]);
        (tmp, server)
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
                    json!({ "name": "get_context", "arguments": [] }),
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
