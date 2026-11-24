//! The CLI twins of the tools: a command runs the tool's own spec parse
//! and handler on a store, without a client, and gets the tool's result or
//! its refusal, so a command and a call refuse alike and write the same
//! bytes. `--json` prints what the call's text holds.

use serde_json::{Map, Value, json};

use super::apply::APPLY;
use super::spec::{Output, to_value};
use super::status::STATUS;
use super::truncate::fit_payload;
use crate::budget::MAX_TOOL_RESULT_BYTES;
use crate::store::{Applied, Store};

pub(crate) use super::protocol::MAX_MESSAGE_BYTES as MAX_DOCUMENT_BYTES;
pub(crate) use super::result::ToolError;
pub(crate) use super::status::{Part, QualityCounts, Report};

/// The arguments `document` holds: a JSON object, as a batch file holds
/// the arguments of `apply`.
pub(crate) fn arguments(document: &[u8]) -> Result<Map<String, Value>, ToolError> {
    match serde_json::from_slice(document) {
        Ok(Value::Object(arguments)) => Ok(arguments),
        Ok(_) => Err(ToolError::bad_document(
            "a batch is a JSON object of `ops` and `origin`".into(),
        )),
        Err(e) => Err(ToolError::bad_document(format!("a batch is JSON: {e}"))),
    }
}

/// `apply` with `arguments`, a batch file's.
pub(crate) fn apply(store: &Store, arguments: &Map<String, Value>) -> Result<Applied, ToolError> {
    APPLY.run(store, arguments)
}

/// `status` with `arguments`, the command's flags.
pub(crate) fn status(store: &Store, arguments: &Map<String, Value>) -> Result<Report, ToolError> {
    STATUS.run(store, arguments)
}

/// The text a call's result holds for `result`.
pub(crate) fn result_json<O: Output>(result: &O) -> String {
    fit_payload(&to_value(result), MAX_TOOL_RESULT_BYTES)
}

/// The text a call's refusal holds for `error`.
pub(crate) fn refusal_json(error: &ToolError) -> String {
    fit_payload(&json!({ "error": error }), MAX_TOOL_RESULT_BYTES)
}
