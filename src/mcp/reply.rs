//! A request's reply: its `result`, a JSON-RPC error, or none for a call
//! the client cancelled.

use serde::Serialize;
use serde_json::Value;

use super::envelope::ToolEnvelope;
use super::protocol::INVALID_PARAMS;

/// What a request is answered with, written once its handling is over.
pub(super) enum Reply {
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
pub(super) enum Body {
    Value(Value),
    Listed(&'static Value),
    Tool(ToolEnvelope),
}

pub(super) fn invalid_params(message: impl Into<String>) -> Reply {
    Reply::Failure {
        code: INVALID_PARAMS,
        message: message.into(),
    }
}
