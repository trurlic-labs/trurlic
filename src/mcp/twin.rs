//! The CLI twins of the tools: a command finds the store as a call does,
//! runs the tool's own spec parse and handler on it, without a client, and
//! gets the tool's result or its refusal, a missing store included, so a
//! command and a call refuse alike and write the same bytes. `--json`
//! prints the text the call returns, rendered by the call's own functions.

use std::path::Path;

use serde_json::{Map, Value};

use super::apply::APPLY;
use super::result::result_text;
use super::spec::to_value;
use super::status::STATUS;
use crate::discovery::discover;

pub(crate) use super::apply::{Answer, Duplicates};
pub(crate) use super::protocol::MAX_MESSAGE_BYTES as MAX_DOCUMENT_BYTES;
pub(crate) use super::result::{ToolError, refusal_text};
pub(crate) use super::spec::Output;
pub(crate) use super::status::{
    Cut, LintItem, PAIRS_MORE, Part, PatternCounts, QualityCounts, Report, section_names,
};

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

/// `apply` with `arguments`, a batch file's, on the store found from
/// `cwd`.
pub(crate) fn apply(cwd: &Path, arguments: &Map<String, Value>) -> Result<Answer, ToolError> {
    APPLY.run(&discover(cwd)?, arguments)
}

/// `status` with `arguments`, the command's flags, on the store found from
/// `cwd`.
pub(crate) fn status(cwd: &Path, arguments: &Map<String, Value>) -> Result<Report, ToolError> {
    STATUS.run(&discover(cwd)?, arguments)
}

/// The text a call's result holds for `result`.
pub(crate) fn result_json<O: Output>(result: &O) -> String {
    result_text(to_value(result)).1
}
