//! What a tool call returns on every surface: a payload, or a refusal in
//! one typed shape, each fitted to [`MAX_TOOL_RESULT_BYTES`] as JSON text.
//!
//! A [`ToolResult`] is built only by its constructors, which fit it, so no
//! surface can send one past the budget. A result built on a spec carries
//! its JSON twice, as a value and as the text that value serializes, so a
//! surface that sends both sends one JSON; a cut result is cut in both.
//!
//! A refusal's text is the JSON object `{"error": {"kind", "field"?, "op"?,
//! "rule"?, "message"}}`; `kind` is what a caller branches on, `field`
//! names the argument at fault, and a refused batch names its op and the
//! rule it breaks.

use std::fmt;

use serde::Serialize;
use serde_json::{Value, json};

use super::truncate;
use crate::Error;
use crate::budget::MAX_TOOL_RESULT_BYTES;
use crate::store::Refusal;

/// Why a tool call was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ErrorKind {
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

    /// An argument refused: by its spec, or by the handler for what the
    /// spec alone cannot hold, a value that holds only beside another.
    pub(super) fn invalid(field: &str, message: impl Into<String>) -> Self {
        Self::new(
            ErrorKind::InvalidArgument,
            Some(field.to_owned()),
            message.into(),
        )
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

/// A tool's answer as a surface sends it: JSON text within
/// [`MAX_TOOL_RESULT_BYTES`].
#[derive(Debug)]
pub(crate) struct ToolResult {
    text: String,
    kind: ResultKind,
}

/// What the text of a [`ToolResult`] is.
#[derive(Debug, PartialEq)]
pub(crate) enum ResultKind {
    /// A result built on a spec, with the JSON value its text serializes.
    Structured(Value),
    /// The result of a read written by hand: its text alone.
    Unstructured,
    /// A refusal: `{"error": ...}`.
    Refusal,
}

impl ToolResult {
    /// `payload`, a result built on a spec, as it is or as it was cut to
    /// fit ([`truncate`]); the text is the serialization of the value kept.
    pub(crate) fn structured(payload: Value) -> Self {
        let (value, text) = truncate::fit_value(payload, MAX_TOOL_RESULT_BYTES);
        Self {
            text,
            kind: ResultKind::Structured(value),
        }
    }

    /// `payload` serialized, cut to fit like any other.
    pub(crate) fn unstructured(payload: &Value) -> Self {
        Self {
            text: truncate::fit_payload(payload, MAX_TOOL_RESULT_BYTES),
            kind: ResultKind::Unstructured,
        }
    }

    /// `error` as `{"error": ...}`, cut like any payload when its message
    /// is long.
    pub(crate) fn refusal(error: &ToolError) -> Self {
        Self {
            text: truncate::fit_payload(&json!({ "error": error }), MAX_TOOL_RESULT_BYTES),
            kind: ResultKind::Refusal,
        }
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// The text, and what it is.
    pub(crate) fn into_parts(self) -> (String, ResultKind) {
        (self.text, self.kind)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::store::{FORMAT_VERSION, FormatVersion};

    fn refusal_payload(result: ToolResult) -> Value {
        let (text, kind) = result.into_parts();
        assert_eq!(kind, ResultKind::Refusal);
        serde_json::from_str(&text).unwrap()
    }

    #[test]
    fn a_refusal_names_its_kind_field_and_message() {
        let invalid = ToolError::invalid("ops[2].name", "`ops[2].name` is required");
        let payload = refusal_payload(ToolResult::refusal(&invalid));
        assert_eq!(
            payload,
            json!({ "error": {
                "kind": "invalid_argument",
                "field": "ops[2].name",
                "message": "`ops[2].name` is required"
            }})
        );

        let refused = ToolError::refused("component `x` does not exist");
        let payload = refusal_payload(ToolResult::refusal(&refused));
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
            let payload = refusal_payload(ToolResult::refusal(&error.into()));
            assert_eq!(payload["error"]["kind"], kind);
            let message = payload["error"]["message"].as_str().unwrap();
            assert!(message.contains(named), "{kind}: {message}");
        }
    }

    #[test]
    fn a_structured_result_holds_the_same_json_twice_cut_or_not() {
        let held = |result: ToolResult| match result.into_parts() {
            (text, ResultKind::Structured(value)) => (text, value),
            (_, kind) => panic!("not structured: {kind:?}"),
        };
        for payload in [
            json!({ "count": 3 }),
            json!({ "items": vec!["x".repeat(100); MAX_TOOL_RESULT_BYTES / 50] }),
        ] {
            let (text, value) = held(ToolResult::structured(payload));
            assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), value);
        }
        let (_, cut) = held(ToolResult::structured(
            json!({ "items": vec!["x".repeat(100); 1000] }),
        ));
        assert!(cut["truncated"].is_array(), "{cut}");
        // Deeper than a JSON reader nests: the text cannot be read back,
        // and the value is still the one it serializes.
        let mut deep = json!({ "text": "x".repeat(MAX_TOOL_RESULT_BYTES) });
        for _ in 0..200 {
            deep = json!({ "nested": deep });
        }
        let (text, value) = held(ToolResult::structured(deep));
        assert!(text.len() <= MAX_TOOL_RESULT_BYTES);
        assert_eq!(value.to_string(), text);
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

        let payload = refusal_payload(ToolResult::refusal(&error.into()));

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
        let refused = ToolError::refused("x".repeat(MAX_TOOL_RESULT_BYTES * 2));
        let result = ToolResult::refusal(&refused);
        assert!(result.text().len() <= MAX_TOOL_RESULT_BYTES);
        let payload = refusal_payload(result);
        assert_eq!(payload["error"]["kind"], "refused");
        assert!(payload["truncated"].is_array(), "{payload}");
    }
}
