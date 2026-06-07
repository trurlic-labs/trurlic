use serde_json::Value;

use crate::store::{ProjectState, Store};

use super::context;
use super::write;

// ── Tool definitions ────────────────────────────────────────────────────────

pub(crate) fn tool_list() -> Value {
    serde_json::json!({
        "tools": [
            {
                "name": "get_context",
                "description": "Get the architectural context for a component. Returns \
                    decisions, project-wide rules, related decisions from connected \
                    components, and an authoritative brief for coding agents.",
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
                        }
                    },
                    "required": ["component"]
                }
            },
            {
                "name": "check_pattern",
                "description": "Check whether a pattern or approach is covered by \
                    existing decisions. Returns matching decisions sorted by relevance.",
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
                    decision counts, patterns, and project-wide decisions.",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "validate_consistency",
                "description": "Full graph integrity check. Same validation as `trurl check`.",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "record_decision",
                "description": "Record a single architectural decision. Validates all \
                    edges before writing. Atomic commit.",
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
                        }
                    },
                    "required": ["component", "choice", "reason"]
                }
            },
            {
                "name": "record_pattern",
                "description": "Record a pattern — a synthesis of multiple decisions \
                    into a reusable rule. Requires at least 2 decisions.",
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
                    other decisions depend on it or a pattern would shrink below 2 members.",
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
            }
        ]
    })
}

// ── Tool dispatch ───────────────────────────────────────────────────────────

pub(crate) fn call_tool(
    store: &Store,
    state: &mut ProjectState,
    name: &str,
    args: &Value,
) -> Value {
    match name {
        // Read tools
        "get_context" => dispatch_get_context(state, args),
        "check_pattern" => dispatch_check_pattern(state, args),
        "get_architecture" => tool_result(&context::get_architecture(state)),
        "validate_consistency" => tool_result(&write::validate_consistency(state)),
        // Write tools
        "record_decision" => match write::record_decision(store, state, args) {
            Ok(v) => tool_result(&v),
            Err(msg) => tool_error(&msg),
        },
        "record_pattern" => match write::record_pattern(store, state, args) {
            Ok(v) => tool_result(&v),
            Err(msg) => tool_error(&msg),
        },
        "remove_decision" => match write::remove_decision(store, state, args) {
            Ok(v) => tool_result(&v),
            Err(msg) => tool_error(&msg),
        },
        _ => tool_error(&format!("unknown tool: {name}")),
    }
}

fn dispatch_get_context(state: &ProjectState, args: &Value) -> Value {
    let component = match args.get("component").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => return tool_error("missing required parameter: component"),
    };
    let task = args.get("task").and_then(|v| v.as_str());
    match context::get_context(state, component, task) {
        Ok(result) => tool_result(&result),
        Err(msg) => tool_error(&msg),
    }
}

fn dispatch_check_pattern(state: &ProjectState, args: &Value) -> Value {
    let description = match args.get("description").and_then(|v| v.as_str()) {
        Some(d) => d,
        None => return tool_error("missing required parameter: description"),
    };
    tool_result(&context::check_pattern(state, description))
}

pub(crate) fn tool_result(payload: &Value) -> Value {
    serde_json::json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(payload)
                .unwrap_or_else(|_| "{}".to_string())
        }]
    })
}

pub(crate) fn tool_error(message: &str) -> Value {
    serde_json::json!({
        "content": [{ "type": "text", "text": message }],
        "isError": true
    })
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
        assert!(names.contains(&"get_context"));
        assert!(names.contains(&"check_pattern"));
        assert!(names.contains(&"get_architecture"));
        assert!(names.contains(&"validate_consistency"));
        assert!(names.contains(&"record_decision"));
        assert!(names.contains(&"record_pattern"));
        assert!(names.contains(&"remove_decision"));
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
    fn tool_result_wraps_in_content_block() {
        let payload = serde_json::json!({"status": "covered"});
        let result = tool_result(&payload);

        let content = result["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "text");
        assert!(result.get("isError").is_none());
    }

    #[test]
    fn tool_error_sets_is_error() {
        let result = tool_error("something broke");
        assert_eq!(result["isError"], true);
        assert_eq!(result["content"][0]["text"], "something broke");
    }

    #[test]
    fn dispatch_unknown_tool_returns_error() {
        let store = Store::at("/dev/null/.trurl".into());
        let mut state = empty_state();
        let result = call_tool(&store, &mut state, "nonexistent", &serde_json::json!({}));
        assert_eq!(result["isError"], true);
    }

    #[test]
    fn dispatch_get_context_missing_component() {
        let store = Store::at("/dev/null/.trurl".into());
        let mut state = empty_state();
        let result = call_tool(&store, &mut state, "get_context", &serde_json::json!({}));
        assert_eq!(result["isError"], true);
    }

    #[test]
    fn dispatch_check_pattern_missing_description() {
        let store = Store::at("/dev/null/.trurl".into());
        let mut state = empty_state();
        let result = call_tool(&store, &mut state, "check_pattern", &serde_json::json!({}));
        assert_eq!(result["isError"], true);
    }

    fn empty_state() -> ProjectState {
        use crate::store::schema::*;
        use chrono::Utc;
        ProjectState::new(
            ProjectFile {
                trurl_version: "0.2.0".into(),
                project: Project {
                    name: "test".into(),
                    description: String::new(),
                },
            },
            std::collections::BTreeMap::new(),
            std::collections::BTreeMap::new(),
            std::collections::BTreeMap::new(),
            GraphIndex {
                version: 1,
                rebuilt: Utc::now(),
                nodes: vec![],
                edges: vec![],
            },
        )
    }
}
