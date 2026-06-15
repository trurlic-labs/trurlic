use std::sync::LazyLock;

use serde::Serialize;
use serde_json::Value;

use super::context;
use super::{update, write};
use crate::store::{ProjectState, Store};
use crate::workflow;

// ── Tool definitions ────────────────────────────────────────────────────────

/// Static tool catalogue. Built once on first access, returned by reference
/// thereafter. Each `tools/list` response clones from this cache instead of
/// rebuilding the schema tree from scratch.
static TOOL_DEFINITIONS: LazyLock<Value> = LazyLock::new(|| {
    serde_json::json!({
        "tools": [
            {
                "name": "advance",
                "description": "Compute the workflow step for a component \
                    and return the next action. Call before acting on a \
                    component and after completing each action. The returned \
                    `ready` field is the only signal that implementation \
                    can proceed.",
                "annotations": {
                    "title": "Advance workflow",
                    "readOnlyHint": true,
                    "destructiveHint": false,
                    "idempotentHint": true,
                    "openWorldHint": false
                },
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "component": {
                            "type": "string",
                            "description": "Component name (kebab-case) or 'project'."
                        },
                        "task_type": {
                            "type": "string",
                            "enum": [
                                "new_component",
                                "feature",
                                "fix",
                                "learn",
                                "review",
                                "harden",
                                "bootstrap"
                            ],
                            "description": "What the developer wants to accomplish. \
                                Inferred from graph state if omitted. \
                                new_component: build from scratch. \
                                feature: add to existing component. \
                                fix: apply a bugfix. \
                                learn: study existing decisions. \
                                review: challenge decisions for drift. \
                                harden: strengthen coverage gaps. \
                                bootstrap: autonomous project scan — agent reads \
                                source code and records components, decisions, \
                                and patterns without interactive dialogue."
                        },
                        "task": {
                            "type": "string",
                            "description": "Optional task context passed through to \
                                design prompts."
                        },
                        "step_evidence": {
                            "type": "object",
                            "additionalProperties": { "type": "string" },
                            "description": "Evidence of user involvement for completed \
                                steps. Keys are step names, values are evidence strings. \
                                Gated (interactive) steps require evidence of at least \
                                20 bytes. Ungated steps accept any value including empty \
                                string. The state machine skips steps present in this \
                                map to progress through steps whose postconditions are \
                                not verifiable from the graph alone."
                        }
                    },
                    "required": ["component"]
                }
            },
            {
                "name": "get_context",
                "description": "Get the architectural context for a component. Returns \
                    decisions, project-wide rules, related decisions from connected \
                    components, and an authoritative brief for coding agents. \
                    Use depth=\"constraints\" for token-efficient mid-implementation \
                    checks (~60-70% fewer tokens).",
                "annotations": {
                    "title": "Get context",
                    "readOnlyHint": true,
                    "destructiveHint": false,
                    "idempotentHint": true,
                    "openWorldHint": false
                },
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "component": {
                            "type": "string",
                            "description": "Component name (kebab-case) or 'project'."
                        },
                        "task": {
                            "type": "string",
                            "description": "Optional current coding task description."
                        },
                        "depth": {
                            "type": "string",
                            "enum": ["full", "constraints"],
                            "description": "full (default): complete context with reasoning. \
                                constraints: choice text only, compact brief, no related \
                                decisions. 60-70% fewer tokens for mid-implementation checks."
                        }
                    },
                    "required": ["component"]
                }
            },
            {
                "name": "check_pattern",
                "description": "Check whether a pattern or approach is covered by \
                    existing decisions. Returns matching decisions sorted by \
                    relevance, or suggested_component if not covered.",
                "annotations": {
                    "title": "Check pattern",
                    "readOnlyHint": true,
                    "destructiveHint": false,
                    "idempotentHint": true,
                    "openWorldHint": false
                },
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "description": {
                            "type": "string",
                            "description": "Pattern or approach to check."
                        }
                    },
                    "required": ["description"]
                }
            },
            {
                "name": "get_architecture",
                "description": "Full architectural overview: components, connections, \
                    decision counts, patterns, and project-wide decisions. \
                    Lists components with zero decisions as needing design.",
                "annotations": {
                    "title": "Get architecture",
                    "readOnlyHint": true,
                    "destructiveHint": false,
                    "idempotentHint": true,
                    "openWorldHint": false
                },
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "validate_consistency",
                "description": "Full graph integrity check. Same validation as `trurlic check`.",
                "annotations": {
                    "title": "Validate consistency",
                    "readOnlyHint": true,
                    "destructiveHint": false,
                    "idempotentHint": true,
                    "openWorldHint": false
                },
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "record_decision",
                "description": "Record a single architectural decision. Validates all \
                    edges before writing. Atomic commit. Returns the decision name, \
                    path, warnings, and any detected pattern opportunities.",
                "annotations": {
                    "title": "Record decision",
                    "readOnlyHint": false,
                    "destructiveHint": false,
                    "idempotentHint": false,
                    "openWorldHint": false
                },
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "component": {
                            "type": "string",
                            "description": "Component name or 'project'."
                        },
                        "choice": {
                            "type": "string",
                            "description": "Concise decision title."
                        },
                        "reason": {
                            "type": "string",
                            "description": "Reasoning behind the decision."
                        },
                        "alternatives": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Rejected options with reasons."
                        },
                        "depends_on": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Decision names this depends on."
                        },
                        "constrains": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Decision names this constrains."
                        },
                        "tags": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Categorical tags for filtering."
                        },
                        "supersedes": {
                            "type": "string",
                            "description": "Decision name being replaced."
                        },
                        "attribution": {
                            "type": "string",
                            "enum": ["user", "agent"],
                            "description": "Who authored this decision: \"user\" (human present) or \"agent\" (autonomous)."
                        }
                    },
                    "required": ["component", "choice", "reason", "attribution"]
                }
            },
            {
                "name": "record_pattern",
                "description": "Record a pattern — a synthesis of multiple decisions \
                    into a reusable rule. Requires at least 2 decisions. Returns the \
                    pattern name (slug) for reference.",
                "annotations": {
                    "title": "Record pattern",
                    "readOnlyHint": false,
                    "destructiveHint": false,
                    "idempotentHint": false,
                    "openWorldHint": false
                },
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": {
                            "type": "string",
                            "description": "Human-readable pattern name."
                        },
                        "description": {
                            "type": "string",
                            "description": "What this pattern means."
                        },
                        "decisions": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Decision names (must all exist, minimum 2)."
                        },
                        "components": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Component names (inferred from decisions if omitted)."
                        },
                        "tags": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Categorical tags."
                        }
                    },
                    "required": ["name", "description", "decisions"]
                }
            },
            {
                "name": "remove_decision",
                "description": "Remove a decision with cascade awareness. Refuses if \
                    other decisions depend on it or a pattern would shrink below \
                    2 members. Reports affected patterns and edges.",
                "annotations": {
                    "title": "Remove decision",
                    "readOnlyHint": false,
                    "destructiveHint": true,
                    "idempotentHint": true,
                    "openWorldHint": false
                },
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": {
                            "type": "string",
                            "description": "Decision filename (without .toml)."
                        }
                    },
                    "required": ["name"]
                }
            },
            {
                "name": "update_decision",
                "description": "Modify an existing decision. 'amend' edits in place \
                    (typo, clarification). 'supersede' creates a new decision \
                    replacing the old one (substantive change). Reports affected \
                    patterns and decisions.",
                "annotations": {
                    "title": "Update decision",
                    "readOnlyHint": false,
                    "destructiveHint": false,
                    "idempotentHint": false,
                    "openWorldHint": false
                },
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": {
                            "type": "string",
                            "description": "Existing decision name."
                        },
                        "mode": {
                            "type": "string",
                            "enum": ["amend", "supersede"],
                            "description": "amend = edit in place; supersede = create replacement."
                        },
                        "choice": {
                            "type": "string",
                            "description": "New choice text (at least one of choice/reason required)."
                        },
                        "reason": {
                            "type": "string",
                            "description": "New reason text."
                        }
                    },
                    "required": ["name", "mode"]
                }
            },
            {
                "name": "get_step_prompt",
                "description": "Get the prompt for a specific workflow step. \
                    Called as directed by advance. Returns system instructions, \
                    component context, and step metadata.",
                "annotations": {
                    "title": "Get step prompt",
                    "readOnlyHint": true,
                    "destructiveHint": false,
                    "idempotentHint": true,
                    "openWorldHint": false
                },
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "component": {
                            "type": "string",
                            "description": "Component name or 'project'."
                        },
                        "step": {
                            "type": "string",
                            "enum": [
                                "register",
                                "define_scope",
                                "analyze_code",
                                "cover_concerns",
                                "walk_decisions",
                                "verify_constraints",
                                "impact_check",
                                "pattern_detection",
                                "summary_gate",
                                "drift_check",
                                "coverage_audit",
                                "scan_project",
                                "extract_decisions",
                                "project_rules",
                                "user_explains",
                                "ready"
                            ],
                            "description": "Workflow step to get the prompt for."
                        },
                        "task": {
                            "type": "string",
                            "description": "Optional task context."
                        },
                        "task_type": {
                            "type": "string",
                            "enum": [
                                "new_component",
                                "feature",
                                "fix",
                                "learn",
                                "review",
                                "harden",
                                "bootstrap"
                            ],
                            "description": "Optional task type for variant prompts."
                        }
                    },
                    "required": ["component", "step"]
                }
            },
            {
                "name": "add_component",
                "description": "Add a new component to the architecture graph.",
                "annotations": {
                    "title": "Add component",
                    "readOnlyHint": false,
                    "destructiveHint": false,
                    "idempotentHint": false,
                    "openWorldHint": false
                },
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": {
                            "type": "string",
                            "description": "Kebab-case component name (e.g. 'rate-limiter')."
                        },
                        "description": {
                            "type": "string",
                            "description": "One-line description of the component's role."
                        }
                    },
                    "required": ["name"]
                }
            },
            {
                "name": "add_connection",
                "description": "Add a directional connection between two components. \
                    Represents data or control flow. Connections provide context \
                    to get_context and get_step_prompt.",
                "annotations": {
                    "title": "Add connection",
                    "readOnlyHint": false,
                    "destructiveHint": false,
                    "idempotentHint": false,
                    "openWorldHint": false
                },
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "from": {
                            "type": "string",
                            "description": "Source component name."
                        },
                        "to": {
                            "type": "string",
                            "description": "Target component name."
                        }
                    },
                    "required": ["from", "to"]
                }
            }
        ]
    })
});

pub(crate) fn tool_list() -> &'static Value {
    &TOOL_DEFINITIONS
}

// ── Tool classification ─────────────────────────────────────────────────────

/// Returns `true` for tools that mutate `ProjectState` and the on-disk store.
/// Used by the MCP dispatch to choose between a read lock (cheap, concurrent)
/// and a write lock (exclusive).
pub(crate) fn is_write_tool(name: &str) -> bool {
    matches!(
        name,
        "record_decision"
            | "record_pattern"
            | "remove_decision"
            | "update_decision"
            | "add_component"
            | "add_connection"
    )
}

// ── Read tool dispatch ──────────────────────────────────────────────────────

/// Dispatch a read-only tool call. Requires only `&ProjectState`.
/// Unknown tool names are handled here (they are not write tools).
pub(crate) fn call_read_tool(state: &ProjectState, name: &str, args: &Value) -> ToolEnvelope {
    match name {
        "advance" => dispatch_advance(state, args),
        "get_context" => dispatch_get_context(state, args),
        "check_pattern" => dispatch_check_pattern(state, args),
        "get_architecture" => tool_result(&context::get_architecture(state)),
        "validate_consistency" => tool_result(&write::validate_consistency(state)),
        "get_step_prompt" => dispatch_get_step_prompt(state, args),
        _ => tool_error(&format!("unknown tool: {name}")),
    }
}

// ── Write tool dispatch ─────────────────────────────────────────────────────

/// Dispatch a mutating tool call. Requires `&mut ProjectState` and `&Store`.
/// Only called for tools where [`is_write_tool`] returns `true`.
pub(crate) fn call_write_tool(
    store: &Store,
    state: &mut ProjectState,
    name: &str,
    args: &Value,
) -> ToolEnvelope {
    let result = match name {
        "record_decision" => write::record_decision(store, state, args),
        "record_pattern" => write::record_pattern(store, state, args),
        "remove_decision" => update::remove_decision(store, state, args),
        "update_decision" => update::update_decision(store, state, args),
        "add_component" => write::add_component(store, state, args),
        "add_connection" => write::add_connection(store, state, args),
        _ => return tool_error(&format!("unhandled write tool: {name}")),
    };
    match result {
        Ok(v) => tool_result(&v),
        Err(msg) => tool_error(&msg),
    }
}

// ── Argument dispatch helpers ───────────────────────────────────────────────

fn dispatch_get_context(state: &ProjectState, args: &Value) -> ToolEnvelope {
    let component = match args.get("component").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => return tool_error("missing required parameter: component"),
    };
    let task = args.get("task").and_then(|v| v.as_str());
    let depth = match args.get("depth").and_then(|v| v.as_str()) {
        Some("constraints") => context::ContextDepth::Constraints,
        _ => context::ContextDepth::Full,
    };
    match context::get_context(state, component, task, depth) {
        Ok(result) => tool_result(&result),
        Err(msg) => tool_error(&msg),
    }
}

fn dispatch_check_pattern(state: &ProjectState, args: &Value) -> ToolEnvelope {
    let description = match args.get("description").and_then(|v| v.as_str()) {
        Some(d) => d,
        None => return tool_error("missing required parameter: description"),
    };
    tool_result(&context::check_pattern(state, description))
}

fn dispatch_get_step_prompt(state: &ProjectState, args: &Value) -> ToolEnvelope {
    let component = match args.get("component").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => return tool_error("missing required parameter: component"),
    };
    let step = match args.get("step").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return tool_error("missing required parameter: step"),
    };
    let task = args.get("task").and_then(|v| v.as_str());

    let task_type = args.get("task_type").and_then(|v| v.as_str());

    let prompt = match workflow::steps::build_step_prompt(state, component, step, task, task_type) {
        Ok(p) => p,
        Err(msg) => return tool_error(&msg),
    };

    let ctx = match context::get_context(state, component, task, context::ContextDepth::Full) {
        Ok(c) => c,
        Err(msg) => return tool_error(&msg),
    };

    let mut result = serde_json::json!({
        "system_instructions": prompt.instructions,
        "context": ctx,
        "step": step,
    });
    if !prompt.focus.is_empty() {
        result["focus"] = serde_json::json!(prompt.focus);
    }
    tool_result(&result)
}

fn dispatch_advance(state: &ProjectState, args: &Value) -> ToolEnvelope {
    let component = match args.get("component").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => return tool_error("missing required parameter: component"),
    };
    let task_type = match args.get("task_type").and_then(|v| v.as_str()) {
        Some(s) => match workflow::TaskType::parse(s) {
            Ok(tt) => Some(tt),
            Err(msg) => return tool_error(&msg),
        },
        None => None,
    };
    let task = args.get("task").and_then(|v| v.as_str());

    let evidence_refs: std::collections::BTreeMap<&str, &str> = args
        .get("step_evidence")
        .and_then(|v| v.as_object())
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.as_str(), s)))
                .collect()
        })
        .unwrap_or_default();

    match workflow::advance::advance(state, component, task_type, task, &evidence_refs) {
        Ok(result) => tool_result(&result),
        Err(msg) => tool_error(&msg),
    }
}

// ── MCP content envelope ────────────────────────────────────────────────────

/// Typed MCP tool-response envelope. Serialized directly into the JSON-RPC
/// response by `protocol::write_success` — no intermediate `Value` tree.
///
/// The MCP spec requires `text` to be a JSON string containing the tool
/// output. `tool_result` serializes the payload `Value` once into `text`;
/// `write_success` then serializes this struct inline when writing the
/// full JSON-RPC response. One serialization pass instead of two.
#[derive(Debug, Serialize)]
pub(crate) struct ToolEnvelope {
    content: [TextBlock; 1],
    /// Present and `true` for errors, omitted for success.
    #[serde(rename = "isError", skip_serializing_if = "Option::is_none")]
    is_error: Option<bool>,
}

#[derive(Debug, Serialize)]
struct TextBlock {
    r#type: &'static str,
    text: String,
}

pub(crate) fn tool_result(payload: &Value) -> ToolEnvelope {
    let text = serde_json::to_string(payload).unwrap_or_else(|e| {
        eprintln!("trurlic: tool result serialization error: {e}");
        "{}".into()
    });
    ToolEnvelope {
        content: [TextBlock {
            r#type: "text",
            text,
        }],
        is_error: None,
    }
}

pub(crate) fn tool_error(message: &str) -> ToolEnvelope {
    ToolEnvelope {
        content: [TextBlock {
            r#type: "text",
            text: message.into(),
        }],
        is_error: Some(true),
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_list_has_all_tools() {
        let list = tool_list();
        let tools = list["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"advance"));
        assert!(names.contains(&"get_context"));
        assert!(names.contains(&"check_pattern"));
        assert!(names.contains(&"get_architecture"));
        assert!(names.contains(&"validate_consistency"));
        assert!(names.contains(&"record_decision"));
        assert!(names.contains(&"record_pattern"));
        assert!(names.contains(&"remove_decision"));
        assert!(names.contains(&"update_decision"));
        assert!(names.contains(&"get_step_prompt"));
        assert!(names.contains(&"add_component"));
        assert!(names.contains(&"add_connection"));
    }

    #[test]
    fn tool_list_schemas_have_required_fields() {
        let list = tool_list();
        let tools = list["tools"].as_array().unwrap();
        for tool in tools {
            assert!(tool.get("name").is_some());
            assert!(tool.get("description").is_some());
            assert!(tool.get("inputSchema").is_some());
            assert_eq!(tool["inputSchema"]["type"], "object");
        }
    }

    #[test]
    fn tool_list_is_stable_across_calls() {
        let a = tool_list();
        let b = tool_list();
        assert_eq!(a, b);
    }

    #[test]
    fn tool_result_wraps_payload_as_json_string() {
        let payload = serde_json::json!({"status": "covered", "count": 3});
        let envelope = tool_result(&payload);

        assert_eq!(envelope.content.len(), 1);
        assert_eq!(envelope.content[0].r#type, "text");
        assert!(envelope.is_error.is_none());

        // text field contains valid JSON matching the original payload
        let parsed: Value = serde_json::from_str(&envelope.content[0].text).unwrap();
        assert_eq!(parsed["status"], "covered");
        assert_eq!(parsed["count"], 3);
    }

    #[test]
    fn tool_result_serializes_to_valid_mcp_envelope() {
        let payload = serde_json::json!({"key": "value"});
        let envelope = tool_result(&payload);
        let wire = serde_json::to_value(&envelope).unwrap();

        assert_eq!(wire["content"][0]["type"], "text");
        assert!(wire.get("isError").is_none());
        // text is a JSON string, not a nested object
        assert!(wire["content"][0]["text"].is_string());
    }

    #[test]
    fn tool_error_sets_is_error_flag() {
        let envelope = tool_error("something broke");
        assert_eq!(envelope.is_error, Some(true));
        assert_eq!(envelope.content[0].text, "something broke");
        assert_eq!(envelope.content[0].r#type, "text");
    }

    #[test]
    fn tool_error_serializes_to_valid_mcp_envelope() {
        let envelope = tool_error("bad input");
        let wire = serde_json::to_value(&envelope).unwrap();
        assert_eq!(wire["isError"], true);
        assert_eq!(wire["content"][0]["text"], "bad input");
    }

    #[test]
    fn is_write_tool_classification() {
        // Write tools.
        assert!(is_write_tool("record_decision"));
        assert!(is_write_tool("record_pattern"));
        assert!(is_write_tool("remove_decision"));
        assert!(is_write_tool("update_decision"));
        assert!(is_write_tool("add_component"));
        assert!(is_write_tool("add_connection"));

        // Read tools.
        assert!(!is_write_tool("advance"));
        assert!(!is_write_tool("get_context"));
        assert!(!is_write_tool("check_pattern"));
        assert!(!is_write_tool("get_architecture"));
        assert!(!is_write_tool("validate_consistency"));
        assert!(!is_write_tool("get_step_prompt"));

        // Unknown.
        assert!(!is_write_tool("nonexistent"));
    }

    #[test]
    fn dispatch_advance_missing_component() {
        let state = empty_state();
        let envelope = call_read_tool(&state, "advance", &serde_json::json!({}));
        assert_eq!(envelope.is_error, Some(true));
    }

    #[test]
    fn dispatch_unknown_read_tool_returns_error() {
        let state = empty_state();
        let envelope = call_read_tool(&state, "nonexistent", &serde_json::json!({}));
        assert_eq!(envelope.is_error, Some(true));
        assert!(envelope.content[0].text.contains("unknown tool"));
    }

    #[test]
    fn dispatch_get_context_missing_component() {
        let state = empty_state();
        let envelope = call_read_tool(&state, "get_context", &serde_json::json!({}));
        assert_eq!(envelope.is_error, Some(true));
    }

    #[test]
    fn dispatch_check_pattern_missing_description() {
        let state = empty_state();
        let envelope = call_read_tool(&state, "check_pattern", &serde_json::json!({}));
        assert_eq!(envelope.is_error, Some(true));
    }

    #[test]
    fn tool_list_has_annotations() {
        let list = tool_list();
        let tools = list["tools"].as_array().unwrap();
        for tool in tools {
            let name = tool["name"].as_str().unwrap();
            assert!(
                tool.get("annotations").is_some(),
                "tool `{name}` missing annotations"
            );
            let ann = &tool["annotations"];
            assert!(ann.get("title").is_some(), "tool `{name}` missing title");
            assert!(
                ann.get("readOnlyHint").is_some(),
                "tool `{name}` missing readOnlyHint"
            );
            assert!(
                ann.get("destructiveHint").is_some(),
                "tool `{name}` missing destructiveHint"
            );
            assert!(
                ann.get("idempotentHint").is_some(),
                "tool `{name}` missing idempotentHint"
            );
            assert!(
                ann.get("openWorldHint").is_some(),
                "tool `{name}` missing openWorldHint"
            );
        }
    }

    #[test]
    fn read_tools_have_readonly_true() {
        let list = tool_list();
        let tools = list["tools"].as_array().unwrap();
        let read_tools = [
            "advance",
            "get_context",
            "check_pattern",
            "get_architecture",
            "validate_consistency",
            "get_step_prompt",
        ];
        for tool in tools {
            let name = tool["name"].as_str().unwrap();
            if read_tools.contains(&name) {
                assert_eq!(
                    tool["annotations"]["readOnlyHint"], true,
                    "tool `{name}` should have readOnlyHint: true"
                );
            }
        }
    }

    #[test]
    fn remove_decision_is_destructive() {
        let list = tool_list();
        let tools = list["tools"].as_array().unwrap();
        let tool = tools
            .iter()
            .find(|t| t["name"] == "remove_decision")
            .unwrap();
        assert_eq!(tool["annotations"]["destructiveHint"], true);
    }

    #[test]
    fn all_tools_closed_world() {
        let list = tool_list();
        let tools = list["tools"].as_array().unwrap();
        for tool in tools {
            let name = tool["name"].as_str().unwrap();
            assert_eq!(
                tool["annotations"]["openWorldHint"], false,
                "tool `{name}` should have openWorldHint: false"
            );
        }
    }

    fn empty_state() -> ProjectState {
        crate::store::testing::empty_project_state()
    }
}
