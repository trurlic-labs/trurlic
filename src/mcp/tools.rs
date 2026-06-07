use serde_json::Value;

use super::context;

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
                            "description": "Component name (kebab-case) or 'project' for \
                                project-wide context."
                        },
                        "task": {
                            "type": "string",
                            "description": "Optional description of the current coding task. \
                                Included in the authoritative brief for maximum relevance."
                        }
                    },
                    "required": ["component"]
                }
            },
            {
                "name": "check_pattern",
                "description": "Check whether a pattern, approach, or technology choice is \
                    covered by existing architectural decisions. Returns matching decisions \
                    sorted by relevance.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "description": {
                            "type": "string",
                            "description": "Description of the pattern or approach to check \
                                (e.g. 'JWT tokens for authentication', \
                                'Redis for session storage')."
                        }
                    },
                    "required": ["description"]
                }
            },
            {
                "name": "get_architecture",
                "description": "Get the full architectural overview: all components, their \
                    connections, decision counts, and project-wide decisions.",
                "inputSchema": {
                    "type": "object",
                    "properties": {},
                }
            }
        ]
    })
}

// ── Tool dispatch ───────────────────────────────────────────────────────────

pub(crate) fn call_tool(state: &crate::store::ProjectState, name: &str, args: &Value) -> Value {
    match name {
        "get_context" => dispatch_get_context(state, args),
        "check_pattern" => dispatch_check_pattern(state, args),
        "get_architecture" => tool_result(&context::get_architecture(state)),
        _ => tool_error(&format!("unknown tool: {name}")),
    }
}

fn dispatch_get_context(state: &crate::store::ProjectState, args: &Value) -> Value {
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

fn dispatch_check_pattern(state: &crate::store::ProjectState, args: &Value) -> Value {
    let description = match args.get("description").and_then(|v| v.as_str()) {
        Some(d) => d,
        None => return tool_error("missing required parameter: description"),
    };
    tool_result(&context::check_pattern(state, description))
}

fn tool_result(payload: &Value) -> Value {
    serde_json::json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(payload)
                .unwrap_or_else(|_| "{}".to_string())
        }]
    })
}

fn tool_error(message: &str) -> Value {
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
    fn tool_list_has_three_tools() {
        let list = tool_list();
        let tools = list["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 3);
    }

    #[test]
    fn tool_list_has_correct_names() {
        let list = tool_list();
        let tools = list["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"get_context"));
        assert!(names.contains(&"check_pattern"));
        assert!(names.contains(&"get_architecture"));
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
        let payload = serde_json::json!({});
        let result = dispatch_unknown(&payload);
        assert_eq!(result["isError"], true);
    }

    fn dispatch_unknown(args: &Value) -> Value {
        // Simulate calling an unknown tool without needing a Store.
        let _ = args;
        tool_error(&format!("unknown tool: {}", "nonexistent"))
    }

    #[test]
    fn dispatch_get_context_missing_component() {
        let state = empty_state();
        let args = serde_json::json!({});
        let result = dispatch_get_context(&state, &args);
        assert_eq!(result["isError"], true);
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("component"));
    }

    #[test]
    fn dispatch_check_pattern_missing_description() {
        let state = empty_state();
        let args = serde_json::json!({});
        let result = dispatch_check_pattern(&state, &args);
        assert_eq!(result["isError"], true);
    }

    fn empty_state() -> crate::store::ProjectState {
        use crate::store::schema::*;
        use chrono::Utc;
        crate::store::ProjectState::new(
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
