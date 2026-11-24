//! What a tool call answers: a payload, or a refusal in one typed shape,
//! each fitted to [`MAX_TOOL_RESULT_BYTES`] and wrapped in the MCP
//! tool-result envelope.
//!
//! A result built on a spec carries its JSON twice, as `structuredContent`
//! and as the text of its one content block, so a client that reads either
//! reads the same value; a cut result is cut in both.
//!
//! A refusal is a result with `isError: true`, never a JSON-RPC error, so
//! the agent reads it and can correct the call. Its text is the JSON
//! object `{"error": {"kind", "field"?, "op"?, "rule"?, "message"}}`;
//! `kind` is what a caller branches on, `field` names the argument at
//! fault, and a refused batch names its op and the rule it breaks.

use std::fmt;

use serde::Serialize;
use serde_json::{Value, json};

use super::spec::InvalidArgument;
use super::truncate;
use crate::Error;
use crate::budget::MAX_TOOL_RESULT_BYTES;
use crate::store::Refusal;

/// Typed MCP tool-result envelope, serialized once, directly into the
/// JSON-RPC response; `text` holds the payload serialized once.
#[derive(Debug, Serialize)]
pub(crate) struct ToolEnvelope {
    content: [TextBlock; 1],
    /// The text's JSON, for a tool with an `outputSchema`.
    #[serde(rename = "structuredContent", skip_serializing_if = "Option::is_none")]
    structured_content: Option<Value>,
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
    /// The store refused a batch whole and wrote nothing: `rule` names the
    /// rule, `op` the op at fault when one is, `field` the argument.
    BatchRefused,
}

/// A refused call.
#[derive(Debug, Serialize)]
pub(crate) struct ToolError {
    kind: ErrorKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    field: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    op: Option<OpRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rule: Option<&'static str>,
    message: String,
}

/// The op of a refused batch.
#[derive(Debug, Serialize)]
struct OpRef {
    index: usize,
    op: &'static str,
    name: String,
}

impl ToolError {
    pub(crate) fn refused(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Refused, None, message.into())
    }

    const fn new(kind: ErrorKind, field: Option<String>, message: String) -> Self {
        Self {
            kind,
            field,
            op: None,
            rule: None,
            message,
        }
    }

    /// A document a CLI twin read that holds no arguments.
    pub(crate) const fn bad_document(message: String) -> Self {
        Self::new(ErrorKind::InvalidArgument, None, message)
    }
}

/// The message: what a command prints after `error: `.
impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<InvalidArgument> for ToolError {
    fn from(invalid: InvalidArgument) -> Self {
        let message = invalid.to_string();
        Self::new(ErrorKind::InvalidArgument, Some(invalid.field), message)
    }
}

impl From<Error> for ToolError {
    fn from(error: Error) -> Self {
        let kind = match error {
            Error::Refused(refusal) => return Self::from(*refusal),
            Error::StoreNotFound(_) => ErrorKind::NoProject,
            // Every way `project.toml` states a format this binary cannot read.
            Error::VersionMismatch { .. }
            | Error::BadFormatVersion { .. }
            | Error::MissingFormatVersion { .. }
            | Error::NoMigration { .. } => ErrorKind::UnsupportedFormat,
            // Every other failure to find, read or write the store.
            _ => ErrorKind::StoreError,
        };
        Self::new(kind, None, error.to_string())
    }
}

impl From<Refusal> for ToolError {
    fn from(refusal: Refusal) -> Self {
        let message = refusal.to_string();
        Self {
            kind: ErrorKind::BatchRefused,
            field: Some(refusal.path()),
            rule: Some(refusal.rule.as_str()),
            op: refusal.op.map(|op| OpRef {
                index: op.index,
                op: op.kind.as_str(),
                name: op.target,
            }),
            message,
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
        structured_content: None,
        is_error: None,
    }
}

/// [`tool_result`] with the text's JSON as `structuredContent` too: the
/// payload as it is, or as it was cut to fit.
pub(crate) fn structured_result(payload: Value) -> ToolEnvelope {
    let text = truncate::fit_payload(&payload, MAX_TOOL_RESULT_BYTES);
    // The cut text is JSON: `fit_payload` builds it from a `Value`.
    let structured = serde_json::from_str(&text).unwrap_or(payload);
    ToolEnvelope {
        content: [TextBlock {
            r#type: "text",
            text,
        }],
        structured_content: Some(structured),
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
        structured_content: None,
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
    fn a_structured_result_holds_the_same_json_twice_cut_or_not() {
        for payload in [
            json!({ "count": 3 }),
            json!({ "items": vec!["x".repeat(100); MAX_TOOL_RESULT_BYTES / 50] }),
        ] {
            let wire = wire(&structured_result(payload));
            let text: Value =
                serde_json::from_str(wire["content"][0]["text"].as_str().unwrap()).unwrap();
            assert_eq!(text, wire["structuredContent"]);
        }
        let cut = wire(&structured_result(
            json!({ "items": vec!["x".repeat(100); 1000] }),
        ));
        assert!(cut["structuredContent"]["truncated"].is_array(), "{cut}");
        assert!(
            wire(&tool_result(&json!({})))
                .get("structuredContent")
                .is_none()
        );
    }

    /// A refused batch names its op, the argument and the rule.
    #[test]
    fn a_refused_batch_names_its_op_field_and_rule() {
        use crate::store::{OpAt, OpKind, Rule};

        let refusal = Refusal {
            op: Some(OpAt {
                index: 3,
                kind: OpKind::Revise,
                target: "sign".into(),
            }),
            field: Some("add_depends_on[0]".into()),
            rule: Rule::SelfEdge,
        };
        let error = Error::Refused(Box::new(refusal));

        let payload = refusal_payload(&tool_error(&error.into()));

        assert_eq!(
            payload,
            json!({ "error": {
                "kind": "batch_refused",
                "field": "ops[3].add_depends_on[0]",
                "op": { "index": 3, "op": "revise", "name": "sign" },
                "rule": "self_edge",
                "message": "ops[3] (revise `sign`), `add_depends_on[0]`: names the node the edge leaves",
            }})
        );
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
