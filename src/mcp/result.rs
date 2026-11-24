//! What a tool call answers: a payload, or a refusal in one typed shape,
//! each fitted to [`MAX_TOOL_RESULT_BYTES`] and wrapped in the MCP
//! tool-result envelope.
//!
//! A refusal is a result with `isError: true`, never a JSON-RPC error, so
//! the agent reads it and can correct the call. Its text is the JSON
//! object `{"error": {"kind", "field"?, "message"}}`; `kind` is what a
//! caller branches on, `field` names the argument at fault.

use serde::Serialize;
use serde_json::{Value, json};

use super::spec::InvalidArgument;
use super::truncate;
use crate::Error;
use crate::budget::MAX_TOOL_RESULT_BYTES;

/// Typed MCP tool-result envelope, serialized once, directly into the
/// JSON-RPC response; `text` holds the payload serialized once.
#[derive(Debug, Serialize)]
pub(crate) struct ToolEnvelope {
    content: [TextBlock; 1],
    /// Present and `true` for a refusal, omitted for success.
    #[serde(rename = "isError", skip_serializing_if = "Option::is_none")]
    is_error: Option<bool>,
}

#[derive(Debug, Serialize)]
struct TextBlock {
    r#type: &'static str,
    text: String,
}

/// Why a tool call was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ErrorKind {
    /// An argument breaks the tool's spec; `field` names it.
    InvalidArgument,
    /// No `.trurlic/` above the server's directory; `trurlic init` makes one.
    NoProject,
    /// The store is in another format; `trurlic migrate` or a newer
    /// trurlic reads it.
    UnsupportedFormat,
    /// The store could not be read or written: I/O, a malformed node file,
    /// a lock held too long.
    StoreError,
    /// The graph or the tool refused the request as it stands.
    Refused,
}

/// A refused call.
#[derive(Debug, Serialize)]
pub(crate) struct ToolError {
    kind: ErrorKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    field: Option<String>,
    message: String,
}

impl ToolError {
    pub(crate) fn refused(message: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::Refused,
            field: None,
            message: message.into(),
        }
    }
}

impl From<InvalidArgument> for ToolError {
    fn from(invalid: InvalidArgument) -> Self {
        Self {
            kind: ErrorKind::InvalidArgument,
            message: invalid.to_string(),
            field: Some(invalid.field),
        }
    }
}

impl From<Error> for ToolError {
    fn from(error: Error) -> Self {
        let kind = match error {
            Error::StoreNotFound(_) => ErrorKind::NoProject,
            // Every way `project.toml` states a format this binary cannot read.
            Error::VersionMismatch { .. }
            | Error::BadFormatVersion { .. }
            | Error::MissingFormatVersion { .. }
            | Error::NoMigration { .. } => ErrorKind::UnsupportedFormat,
            // Every other failure to find, read or write the store.
            _ => ErrorKind::StoreError,
        };
        Self {
            kind,
            field: None,
            message: error.to_string(),
        }
    }
}

/// `payload` serialized within [`MAX_TOOL_RESULT_BYTES`]; see [`truncate`].
pub(crate) fn tool_result(payload: &Value) -> ToolEnvelope {
    ToolEnvelope {
        content: [TextBlock {
            r#type: "text",
            text: truncate::fit_payload(payload, MAX_TOOL_RESULT_BYTES),
        }],
        is_error: None,
    }
}

/// `error` as a refusal, cut like any payload when its message is long.
pub(crate) fn tool_error(error: &ToolError) -> ToolEnvelope {
    ToolEnvelope {
        content: [TextBlock {
            r#type: "text",
            text: truncate::fit_payload(&json!({ "error": error }), MAX_TOOL_RESULT_BYTES),
        }],
        is_error: Some(true),
    }
}

/// [`tool_error`] for a refusal its tool states as text alone.
pub(crate) fn refusal(message: &str) -> ToolEnvelope {
    tool_error(&ToolError::refused(message))
}

#[cfg(test)]
impl ToolEnvelope {
    pub(crate) fn is_error(&self) -> bool {
        self.is_error == Some(true)
    }

    pub(crate) fn text(&self) -> &str {
        &self.content[0].text
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::mcp::spec::Fault;
    use crate::store::{FORMAT_VERSION, FormatVersion};

    fn wire(envelope: &ToolEnvelope) -> Value {
        serde_json::to_value(envelope).unwrap()
    }

    fn refusal_payload(envelope: &ToolEnvelope) -> Value {
        let wire = wire(envelope);
        assert_eq!(wire["isError"], true);
        assert_eq!(wire["content"][0]["type"], "text");
        serde_json::from_str(wire["content"][0]["text"].as_str().unwrap()).unwrap()
    }

    #[test]
    fn a_result_is_its_payload_as_one_text_block() {
        let wire = wire(&tool_result(&json!({ "count": 3 })));
        assert!(wire.get("isError").is_none());
        assert_eq!(wire["content"][0]["type"], "text");
        assert_eq!(wire["content"][0]["text"], r#"{"count":3}"#);
    }

    #[test]
    fn a_refusal_names_its_kind_field_and_message() {
        let invalid = InvalidArgument {
            field: "ops[2].name".into(),
            fault: Fault::Missing,
        };
        let payload = refusal_payload(&tool_error(&invalid.into()));
        assert_eq!(
            payload,
            json!({ "error": {
                "kind": "invalid_argument",
                "field": "ops[2].name",
                "message": "`ops[2].name` is required"
            }})
        );

        let payload = refusal_payload(&refusal("component `x` does not exist"));
        assert_eq!(
            payload,
            json!({ "error": { "kind": "refused", "message": "component `x` does not exist" } })
        );
    }

    #[test]
    fn a_store_error_keeps_its_kind_and_message() {
        let cases = [
            (
                Error::StoreNotFound(PathBuf::from("/work")),
                "no_project",
                "trurlic init",
            ),
            (
                Error::VersionMismatch {
                    path: PathBuf::from("/work/.trurlic/project.toml"),
                    found: FormatVersion::new(0, 4, 0),
                    expected: FORMAT_VERSION,
                },
                "unsupported_format",
                "trurlic migrate",
            ),
            (
                Error::BadFormatVersion {
                    path: PathBuf::from("/work/.trurlic/project.toml"),
                    text: "0.4.one".into(),
                },
                "unsupported_format",
                "`0.4.one`",
            ),
            (
                Error::MissingFormatVersion {
                    path: PathBuf::from("/work/.trurlic/project.toml"),
                },
                "unsupported_format",
                "format_version",
            ),
            (
                Error::NoMigration {
                    path: PathBuf::from("/work/.trurlic/project.toml"),
                    found: FormatVersion::new(0, 3, 7),
                    expected: FormatVersion::new(0, 5, 0),
                },
                "unsupported_format",
                "no migration",
            ),
            (
                Error::LockTimeout {
                    timeout_secs: 5,
                    detail: "held".into(),
                },
                "store_error",
                "lock",
            ),
        ];
        for (error, kind, named) in cases {
            let payload = refusal_payload(&tool_error(&error.into()));
            assert_eq!(payload["error"]["kind"], kind);
            let message = payload["error"]["message"].as_str().unwrap();
            assert!(message.contains(named), "{kind}: {message}");
        }
    }

    #[test]
    fn a_long_refusal_fits_the_budget_and_stays_json() {
        let envelope = refusal(&"x".repeat(MAX_TOOL_RESULT_BYTES * 2));
        let text = wire(&envelope)["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(text.len() <= MAX_TOOL_RESULT_BYTES);
        let payload: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(payload["error"]["kind"], "refused");
        assert!(payload["truncated"].is_array(), "{payload}");
    }
}
