use std::sync::LazyLock;

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
                                "harden"
                            ],
                            "description": "What the developer wants to accomplish. \
                                Inferred from graph state if omitted. \
                                new_component: build from scratch. \
                                feature: add to existing component. \
                                fix: apply a bugfix. \
                                learn: study existing decisions. \
                                review: challenge decisions for drift. \
                                harden: strengthen coverage gaps."
                        },
                        "task": {
                            "type": "string",
                            "description": "Optional task context passed through to \
                                design prompts."
                        },
                        "completed_steps": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Steps already completed without graph \
                                changes. The state machine skips these to progress \
                                through steps whose postconditions are not verifiable \
                                from the graph alone (e.g. verify_constraints, \
                                walk_decisions, coverage_audit, summary_gate)."
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
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "validate_consistency",
                "description": "Full graph integrity check. Same validation as `trurlic check`.",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "record_decision",
                "description": "Record a single architectural decision. Validates all \
                    edges before writing. Atomic commit. Returns the decision name, \
                    path, warnings, and any detected pattern opportunities.",
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
                    into a reusable rule. Requires at least 2 decisions. Returns the \
                    pattern name (slug) for reference.",
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
                                "ready"
                            ],
                            "description": "Workflow step to get the prompt for."
                        },
                        "task": {
                            "type": "string",
                            "description": "Optional task context."
                        }
                    },
                    "required": ["component", "step"]
                }
            },
            {
                "name": "add_component",
                "description": "Add a new component to the architecture graph.",
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
pub(crate) fn call_read_tool(state: &ProjectState, name: &str, args: &Value) -> Value {
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
) -> Value {
    let result = match name {
        "record_decision" => write::record_decision(store, state, args),
        "record_pattern" => write::record_pattern(store, state, args),
        "remove_decision" => update::remove_decision(store, state, args),
        "update_decision" => update::update_decision(store, state, args),
        "add_component" => write::add_component(store, state, args),
        "add_connection" => write::add_connection(store, state, args),
        _ => unreachable!("is_write_tool gate prevents unknown tools here"),
    };
    match result {
        Ok(v) => tool_result(&v),
        Err(msg) => tool_error(&msg),
    }
}

// ── Argument dispatch helpers ───────────────────────────────────────────────

fn dispatch_get_context(state: &ProjectState, args: &Value) -> Value {
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

fn dispatch_check_pattern(state: &ProjectState, args: &Value) -> Value {
    let description = match args.get("description").and_then(|v| v.as_str()) {
        Some(d) => d,
        None => return tool_error("missing required parameter: description"),
    };
    tool_result(&context::check_pattern(state, description))
}

fn dispatch_get_step_prompt(state: &ProjectState, args: &Value) -> Value {
    let component = match args.get("component").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => return tool_error("missing required parameter: component"),
    };
    let step = match args.get("step").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return tool_error("missing required parameter: step"),
    };
    let task = args.get("task").and_then(|v| v.as_str());

    let prompt = match workflow::steps::build_step_prompt(state, component, step, task) {
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

fn dispatch_advance(state: &ProjectState, args: &Value) -> Value {
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

    let completed_owned: Vec<String> = args
        .get("completed_steps")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let completed_refs: Vec<&str> = completed_owned.iter().map(|s| s.as_str()).collect();

    match workflow::advance::advance(state, component, task_type, task, &completed_refs) {
        Ok(result) => tool_result(&result),
        Err(msg) => tool_error(&msg),
    }
}

pub(crate) fn tool_result(payload: &Value) -> Value {
    serde_json::json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string(payload)
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
        let result = call_read_tool(&state, "advance", &serde_json::json!({}));
        assert_eq!(result["isError"], true);
    }

    #[test]
    fn dispatch_unknown_read_tool_returns_error() {
        let state = empty_state();
        let result = call_read_tool(&state, "nonexistent", &serde_json::json!({}));
        assert_eq!(result["isError"], true);
    }

    #[test]
    fn dispatch_get_context_missing_component() {
        let state = empty_state();
        let result = call_read_tool(&state, "get_context", &serde_json::json!({}));
        assert_eq!(result["isError"], true);
    }

    #[test]
    fn dispatch_check_pattern_missing_description() {
        let state = empty_state();
        let result = call_read_tool(&state, "check_pattern", &serde_json::json!({}));
        assert_eq!(result["isError"], true);
    }

    fn empty_state() -> ProjectState {
        crate::store::testing::empty_project_state()
    }
}
