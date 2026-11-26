//! A `tools/call` request: the tool it names, called with its arguments
//! on the store found from the server's directory, and the tool's result
//! as the reply.

use std::path::Path;
use std::sync::LazyLock;

use serde_json::{Map, Value, json};

#[cfg(feature = "failpoints")]
use crate::failpoint::{self, Site};
#[cfg(feature = "failpoints")]
use crate::tools::ToolError;
use crate::tools::{Tool, ToolResult};

use super::peer::Peer;
use super::reply::{Body, Reply, invalid_params};

static NO_ARGUMENTS: LazyLock<Value> = LazyLock::new(|| json!({}));

/// A tool listed in `tools/list`, called on the store found from `cwd`. A
/// malformed call or an unknown tool is a protocol error; anything the
/// tool refuses, a missing store included, is a tool result with
/// `isError`; a call the client cancelled while it waited gets no answer.
pub(super) fn call_tool(
    cwd: &Path,
    params: Option<&Map<String, Value>>,
    peer: &mut Peer<'_>,
) -> Reply {
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
    match peer.request(super::peer::ServerMethod::CreateElicitation, &form) {
        Ok(answer) => ToolResult::unstructured(&answer),
        Err(e) => ToolResult::refusal(&ToolError::refused(e.to_string())),
    }
}
