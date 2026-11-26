//! The MCP tool-result envelope: a [`ToolResult`] as `tools/call` answers
//! it, serialized once, directly into the JSON-RPC response.
//!
//! The result's text is its one content block. A result built on a spec
//! sends its value as `structuredContent` too, the value that text
//! serializes. A refusal is a result with `isError: true`, never a
//! JSON-RPC error, so the agent reads it and can correct the call.

use serde::Serialize;
use serde_json::Value;

use crate::mcp::result::{ResultKind, ToolResult};

/// Typed MCP tool-result envelope; `text` holds the result's text as the
/// tool layer fitted it.
#[derive(Debug, Serialize)]
pub(super) struct ToolEnvelope {
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

impl From<ToolResult> for ToolEnvelope {
    fn from(result: ToolResult) -> Self {
        let (text, kind) = result.into_parts();
        let (structured_content, is_error) = match kind {
            ResultKind::Structured(value) => (Some(value), None),
            ResultKind::Unstructured => (None, None),
            ResultKind::Refusal => (None, Some(true)),
        };
        Self {
            content: [TextBlock {
                r#type: "text",
                text,
            }],
            structured_content,
            is_error,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::mcp::result::ToolError;

    fn wire(result: ToolResult) -> Value {
        serde_json::to_value(ToolEnvelope::from(result)).unwrap()
    }

    #[test]
    fn a_result_is_its_payload_as_one_text_block() {
        let wire = wire(ToolResult::unstructured(&json!({ "count": 3 })));
        assert_eq!(
            wire,
            json!({ "content": [{ "type": "text", "text": r#"{"count":3}"# }] })
        );
    }

    #[test]
    fn a_structured_result_sends_its_value_beside_its_text() {
        let result = ToolResult::structured(json!({ "count": 3 }));
        let text = result.text().to_owned();
        let wire = wire(result);
        assert_eq!(wire["content"][0]["text"], text);
        assert_eq!(wire["structuredContent"], json!({ "count": 3 }));
        assert!(wire.get("isError").is_none(), "{wire}");
    }

    #[test]
    fn a_refusal_is_marked_is_error_and_carries_no_value() {
        let refused = ToolResult::refusal(&ToolError::refused("no"));
        let text = refused.text().to_owned();
        let wire = wire(refused);
        assert_eq!(wire["isError"], true);
        assert_eq!(wire["content"][0]["text"], text);
        assert!(wire.get("structuredContent").is_none(), "{wire}");
    }
}
