use std::sync::LazyLock;

use serde_json::Value;

use super::context;
use super::result::{ToolEnvelope, refusal, tool_result};
use super::{pattern, spec, update, verify, write};
use crate::store::{ProjectState, Writer};

// ── Tool definitions ────────────────────────────────────────────────────────

/// The `tools/list` result: every spec-built tool, then the tools that
/// predate specs. Built once on first access, returned by reference.
static TOOL_DEFINITIONS: LazyLock<Value> = LazyLock::new(|| {
    let mut tools: Vec<Value> = spec::TOOLS.iter().map(|tool| tool.definition()).collect();
    tools.extend(unspecified_tools());
    serde_json::json!({ "tools": tools })
});

/// The tools whose schemas are written out by hand, each parsing its own
/// arguments below.
fn unspecified_tools() -> Vec<Value> {
    let Value::Array(tools) = serde_json::json!([
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
                "name": "get_decision_history",
                "description": "Trace how a decision evolved. Returns its \
                    current choice, reason, attribution, and creation time, \
                    plus every prior version in chronological order (oldest \
                    first) and a count of revisions.",
                "annotations": {
                    "title": "Get decision history",
                    "readOnlyHint": true,
                    "destructiveHint": false,
                    "idempotentHint": true,
                    "openWorldHint": false
                },
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": {
                            "type": "string",
                            "description": "Decision name (without .toml)."
                        }
                    },
                    "required": ["name"]
                }
            },
            {
                "name": "get_decisions_for_file",
                "description": "Reverse lookup: given a file path, find every decision \
                    whose code_refs reference it (exact match or directory prefix). \
                    Returns decisions sorted by component then name. Does NOT include \
                    project-wide rules — they apply everywhere; pair with get_context \
                    for full coverage.",
                "annotations": {
                    "title": "Get decisions for file",
                    "readOnlyHint": true,
                    "destructiveHint": false,
                    "idempotentHint": true,
                    "openWorldHint": false
                },
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "file": {
                            "type": "string",
                            "description": "Relative file path or directory from project root \
                                (e.g. 'src/store/write.rs' or 'src/store'). \
                                Leading './' is stripped; backslashes, traversal (..), \
                                and absolute paths are rejected."
                        }
                    },
                    "required": ["file"]
                }
            },
            {
                "name": "verify_against_decisions",
                "description": "Call after implementing code, before committing. \
                    Returns architectural decisions that apply to the files you \
                    changed, with instructions to verify your code respects them. \
                    Read each affected file and evaluate.",
                "annotations": {
                    "title": "Verify against decisions",
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
                        "changed_files": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Relative file paths from project root that \
                                you changed (e.g. 'src/store/write.rs'). Leading './' is \
                                stripped; backslashes, traversal (..), and absolute paths \
                                are rejected."
                        }
                    },
                    "required": ["component", "changed_files"]
                }
            },
            {
                "name": "record_decision",
                "description": "Record a single architectural decision. Validates all \
                    edges before writing. Atomic commit. Returns the decision name, \
                    path, its code refs, and warnings.",
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
                        "attribution": {
                            "type": "string",
                            "enum": ["user", "agent"],
                            "description": "Who authored this decision: \"user\" (human present) or \"agent\" (autonomous)."
                        },
                        "code_refs": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "file": {
                                        "type": "string",
                                        "description": "Relative path from project root (e.g. 'src/store/write.rs')."
                                    },
                                    "symbol": {
                                        "type": "string",
                                        "description": "Function, struct, constant, or method name."
                                    }
                                },
                                "required": ["file"]
                            },
                            "description": "Source code locations where this decision manifests. An agent-attributed decision should always include these."
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
                "name": "remove_pattern",
                "description": "Remove a pattern and its member_of and applies_to \
                    edges. Member decisions are kept. Remove the pattern first when \
                    its members must go, since a pattern cannot drop below 2 members.",
                "annotations": {
                    "title": "Remove pattern",
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
                            "description": "Pattern name (the slug record_pattern returned)."
                        }
                    },
                    "required": ["name"]
                }
            },
            {
                "name": "update_decision",
                "description": "Modify an existing decision in place. 'revise' \
                    updates content and versions the previous choice/reason into \
                    history. 'promote' marks an agent decision as human-reviewed \
                    by changing its attribution to user. The decision's name and \
                    every edge survive unchanged.",
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
                            "enum": ["revise", "promote"],
                            "description": "revise: update content, previous version saved to history. promote: mark an agent decision as human-reviewed. Call ONLY when the user has explicitly reviewed the decision and confirmed it in this conversation. Never promote autonomously."
                        },
                        "choice": {
                            "type": "string",
                            "description": "New choice text (revise only, optional — omit to keep current)."
                        },
                        "reason": {
                            "type": "string",
                            "description": "New reason text (revise only, optional — omit to keep current)."
                        },
                        "tags": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "New tags (revise only, optional — omit to keep current)."
                        },
                        "code_refs": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "file": {
                                        "type": "string",
                                        "description": "Relative path from project root (e.g. 'src/store/write.rs')."
                                    },
                                    "symbol": {
                                        "type": "string",
                                        "description": "Function, struct, constant, or method name."
                                    }
                                },
                                "required": ["file"]
                            },
                            "description": "New code refs (revise only, optional — omit to keep current)."
                        }
                    },
                    "required": ["name", "mode"]
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
                    Represents data or control flow. A connected component's \
                    decisions appear as related decisions in get_context.",
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
    ]) else {
        // A `json!` array literal is always a `Value::Array`.
        return Vec::new();
    };
    tools
}

pub(crate) fn tool_list() -> &'static Value {
    &TOOL_DEFINITIONS
}

/// Whether `tools/list` lists `name`; a call to any other name is refused
/// as a protocol error.
pub(crate) fn is_listed(name: &str) -> bool {
    TOOL_DEFINITIONS["tools"]
        .as_array()
        .is_some_and(|tools| tools.iter().any(|tool| tool["name"] == name))
}

// ── Tool classification ─────────────────────────────────────────────────────

/// Returns `true` for tools that mutate the on-disk store. Used by the MCP
/// dispatch to choose between a load under the shared lock and a
/// [`Writer`] under the exclusive one.
pub(crate) fn is_write_tool(name: &str) -> bool {
    matches!(
        name,
        "record_decision"
            | "record_pattern"
            | "remove_decision"
            | "remove_pattern"
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
        "get_context" => dispatch_get_context(state, args),
        "check_pattern" => dispatch_check_pattern(state, args),
        "get_decision_history" => dispatch_get_decision_history(state, args),
        "get_decisions_for_file" => dispatch_get_decisions_for_file(state, args),
        "verify_against_decisions" => dispatch_verify_against_decisions(state, args),
        "get_architecture" => tool_result(&context::get_architecture(state)),
        _ => refusal(&format!("unknown tool: {name}")),
    }
}

// ── Write tool dispatch ─────────────────────────────────────────────────────

/// Dispatch a mutating tool call to `writer`. Only called for tools where
/// [`is_write_tool`] returns `true`.
pub(crate) fn call_write_tool(writer: &mut Writer<'_>, name: &str, args: &Value) -> ToolEnvelope {
    let result = match name {
        "record_decision" => write::record_decision(writer, args),
        "record_pattern" => write::record_pattern(writer, args),
        "remove_decision" => update::remove_decision(writer, args),
        "remove_pattern" => pattern::remove_pattern(writer, args),
        "update_decision" => update::update_decision(writer, args),
        "add_component" => write::add_component(writer, args),
        "add_connection" => write::add_connection(writer, args),
        _ => return refusal(&format!("unhandled write tool: {name}")),
    };
    match result {
        Ok(v) => tool_result(&v),
        Err(msg) => refusal(&msg),
    }
}

// ── Argument dispatch helpers ───────────────────────────────────────────────

fn dispatch_get_context(state: &ProjectState, args: &Value) -> ToolEnvelope {
    let component = match args.get("component").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => return refusal("missing required parameter: component"),
    };
    let task = args.get("task").and_then(|v| v.as_str());
    let depth = match args.get("depth").and_then(|v| v.as_str()) {
        Some("constraints") => context::ContextDepth::Constraints,
        _ => context::ContextDepth::Full,
    };
    match context::get_context(state, component, task, depth) {
        Ok(result) => tool_result(&result),
        Err(msg) => refusal(&msg),
    }
}

fn dispatch_check_pattern(state: &ProjectState, args: &Value) -> ToolEnvelope {
    let description = match args.get("description").and_then(|v| v.as_str()) {
        Some(d) => d,
        None => return refusal("missing required parameter: description"),
    };
    tool_result(&context::check_pattern(state, description))
}

fn dispatch_get_decision_history(state: &ProjectState, args: &Value) -> ToolEnvelope {
    let name = match args.get("name").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => return refusal("missing required parameter: name"),
    };
    match context::get_decision_history(state, name) {
        Ok(result) => tool_result(&result),
        Err(msg) => refusal(&msg),
    }
}

fn dispatch_get_decisions_for_file(state: &ProjectState, args: &Value) -> ToolEnvelope {
    let file = match args.get("file").and_then(|v| v.as_str()) {
        Some(f) => f,
        None => return refusal("missing required parameter: file"),
    };
    match context::get_decisions_for_file(state, file) {
        Ok(result) => tool_result(&result),
        Err(msg) => refusal(&msg),
    }
}

fn dispatch_verify_against_decisions(state: &ProjectState, args: &Value) -> ToolEnvelope {
    let component = match args.get("component").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => return refusal("missing required parameter: component"),
    };

    let entries = match args.get("changed_files").and_then(|v| v.as_array()) {
        Some(arr) if !arr.is_empty() => arr,
        Some(_) => return refusal("changed_files must be a non-empty array"),
        None => {
            return refusal("missing required parameter: changed_files (array of strings)");
        }
    };

    // Normalize and validate every path up front — an invalid path is an
    // error, never a silent skip, matching get_decisions_for_file.
    let mut changed_files: Vec<String> = Vec::with_capacity(entries.len());
    for entry in entries {
        let path = match entry.as_str() {
            Some(p) => p,
            None => return refusal("changed_files entries must be strings"),
        };
        match crate::store::normalize_file_query(path) {
            Ok(normalized) => changed_files.push(normalized),
            Err(e) => return refusal(&format!("invalid file path: {e}")),
        }
    }

    // Component must exist (or be the project scope), matching get_context.
    if component != "project" && !state.components.contains_key(component) {
        return refusal(&format!("component `{component}` does not exist"));
    }

    tool_result(&verify::build_response(
        state.graph(),
        component,
        &changed_files,
    ))
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_list_has_exactly_the_thirteen_tools() {
        let list = tool_list();
        let tools = list["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(
            names,
            [
                "get_context",
                "check_pattern",
                "get_architecture",
                "get_decision_history",
                "get_decisions_for_file",
                "verify_against_decisions",
                "record_decision",
                "record_pattern",
                "remove_decision",
                "remove_pattern",
                "update_decision",
                "add_component",
                "add_connection",
            ]
        );
    }

    #[test]
    fn the_read_only_hint_matches_the_lock_each_tool_takes() {
        for tool in tool_list()["tools"].as_array().unwrap() {
            let name = tool["name"].as_str().unwrap();
            assert_eq!(
                tool["annotations"]["readOnlyHint"],
                !is_write_tool(name),
                "{name}"
            );
        }
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

    /// Claude Code cuts a tool description at 4,096 characters; half of
    /// that keeps a session's fixed cost small.
    #[test]
    fn every_listed_description_fits_its_surface_budget() {
        const MAX_DESCRIPTION_CHARS: usize = 2048;
        for tool in tool_list()["tools"].as_array().unwrap() {
            let description = tool["description"].as_str().unwrap();
            assert!(
                description.chars().count() <= MAX_DESCRIPTION_CHARS,
                "{}: {} chars",
                tool["name"],
                description.chars().count()
            );
        }
    }

    #[test]
    fn is_listed_holds_exactly_the_listed_names() {
        for tool in tool_list()["tools"].as_array().unwrap() {
            assert!(is_listed(tool["name"].as_str().unwrap()));
        }
        assert!(!is_listed("advance"));
        assert!(!is_listed(""));
    }

    #[test]
    fn tool_list_is_stable_across_calls() {
        let a = tool_list();
        let b = tool_list();
        assert_eq!(a, b);
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
        assert!(!is_write_tool("get_context"));
        assert!(!is_write_tool("check_pattern"));
        assert!(!is_write_tool("get_decision_history"));
        assert!(!is_write_tool("get_decisions_for_file"));
        assert!(!is_write_tool("verify_against_decisions"));
        assert!(!is_write_tool("get_architecture"));

        // Unknown.
        assert!(!is_write_tool("nonexistent"));
    }

    #[test]
    fn dispatch_unknown_read_tool_returns_error() {
        let state = empty_state();
        let envelope = call_read_tool(&state, "nonexistent", &serde_json::json!({}));
        assert!(envelope.is_error());
        assert!(envelope.text().contains("unknown tool"));
    }

    #[test]
    fn dispatch_get_context_missing_component() {
        let state = empty_state();
        let envelope = call_read_tool(&state, "get_context", &serde_json::json!({}));
        assert!(envelope.is_error());
    }

    #[test]
    fn dispatch_check_pattern_missing_description() {
        let state = empty_state();
        let envelope = call_read_tool(&state, "check_pattern", &serde_json::json!({}));
        assert!(envelope.is_error());
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

    // ── T05: promote guardrails in tool schema ─────────────────────

    #[test]
    fn update_decision_promote_description_prohibits_autonomous_use() {
        let list = tool_list();
        let tools = list["tools"].as_array().unwrap();
        let tool = tools
            .iter()
            .find(|t| t["name"] == "update_decision")
            .unwrap();
        let mode_desc = tool["inputSchema"]["properties"]["mode"]["description"]
            .as_str()
            .unwrap();
        assert!(
            mode_desc.contains("ONLY when the user has explicitly reviewed"),
            "promote mode description must require explicit user review: {mode_desc}"
        );
        assert!(
            mode_desc.contains("Never promote autonomously"),
            "promote mode description must prohibit autonomous promotion: {mode_desc}"
        );
    }

    // ── T09: get_decisions_for_file dispatch tests ──────────────────

    #[test]
    fn dispatch_get_decisions_for_file_missing_param() {
        let state = empty_state();
        let envelope = call_read_tool(&state, "get_decisions_for_file", &serde_json::json!({}));
        assert!(envelope.is_error());
        assert!(envelope.text().contains("file"));
    }

    #[test]
    fn dispatch_get_decisions_for_file_invalid_path() {
        let state = empty_state();
        let args = serde_json::json!({ "file": "/etc/passwd" });
        let envelope = call_read_tool(&state, "get_decisions_for_file", &args);
        assert!(envelope.is_error(), "absolute path should be rejected");
        assert!(
            envelope.text().contains("invalid file path"),
            "error should describe the issue: {}",
            envelope.text()
        );
    }

    #[test]
    fn dispatch_get_decisions_for_file_no_match_returns_empty() {
        let state = empty_state();
        let args = serde_json::json!({ "file": "src/store/write.rs" });
        let envelope = call_read_tool(&state, "get_decisions_for_file", &args);
        assert!(!envelope.is_error());
        let result: serde_json::Value = serde_json::from_str(envelope.text()).unwrap();
        assert_eq!(result["count"], 0);
        assert!(result["decisions"].as_array().unwrap().is_empty());
    }

    #[test]
    fn dispatch_get_decisions_for_file_finds_matching_decision() {
        use crate::store::Timestamp;
        use crate::store::schema::*;

        let ts = "2025-06-01T10:00:00Z".parse::<Timestamp>().unwrap();
        let mut state = empty_state();
        state.components.insert(
            "store".into(),
            std::sync::Arc::new(ComponentFile {
                component: Component {
                    name: "store".into(),
                    description: "The store".into(),
                },
            }),
        );
        state.decisions.insert(
            "atomic-writes".into(),
            std::sync::Arc::new(DecisionFile {
                decision: Decision {
                    component: "store".into(),
                    choice: "Atomic writes via temp + rename".into(),
                    reason: "Crash safety".into(),
                    alternatives: vec![],
                    tags: vec!["reliability".into()],
                    attribution: Attribution::User,
                    created: ts,
                    code_refs: vec![CodeRef {
                        file: "src/store/write.rs".into(),
                        symbol: Some("commit_with_graph".into()),
                    }],
                    history: vec![],
                },
            }),
        );
        state.graph_index.nodes.push(NodeEntry {
            name: "store".into(),
            kind: NodeKind::Component,
            tags: vec![],
            hash: String::new(),
        });
        state.graph_index.nodes.push(NodeEntry {
            name: "atomic-writes".into(),
            kind: NodeKind::Decision,
            tags: vec![],
            hash: String::new(),
        });
        state.graph_index.edges.push(EdgeEntry {
            from: "atomic-writes".into(),
            to: "store".into(),
            kind: EdgeKind::BelongsTo,
        });
        state.rebuild_graph();

        let args = serde_json::json!({ "file": "src/store/write.rs" });
        let envelope = call_read_tool(&state, "get_decisions_for_file", &args);
        assert!(!envelope.is_error(), "should succeed");
        let result: serde_json::Value = serde_json::from_str(envelope.text()).unwrap();
        assert_eq!(result["count"], 1);
        let decisions = result["decisions"].as_array().unwrap();
        assert_eq!(decisions[0]["name"], "atomic-writes");
        assert_eq!(decisions[0]["component"], "store");
        assert_eq!(decisions[0]["attribution"], "user");
        assert!(
            decisions[0]["tags"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("reliability"))
        );
        let refs = decisions[0]["matching_refs"].as_array().unwrap();
        assert_eq!(refs[0]["file"], "src/store/write.rs");
        assert_eq!(refs[0]["symbol"], "commit_with_graph");
    }

    #[test]
    fn dispatch_get_decisions_for_file_directory_prefix() {
        use crate::store::Timestamp;
        use crate::store::schema::*;

        let ts = "2025-06-01T10:00:00Z".parse::<Timestamp>().unwrap();
        let mut state = empty_state();
        state.components.insert(
            "store".into(),
            std::sync::Arc::new(ComponentFile {
                component: Component {
                    name: "store".into(),
                    description: "The store".into(),
                },
            }),
        );
        state.decisions.insert(
            "atomic-writes".into(),
            std::sync::Arc::new(DecisionFile {
                decision: Decision {
                    component: "store".into(),
                    choice: "Atomic writes".into(),
                    reason: "Crash safety".into(),
                    alternatives: vec![],
                    tags: vec![],
                    attribution: Attribution::Agent,
                    created: ts,
                    code_refs: vec![CodeRef {
                        file: "src/store/write.rs".into(),
                        symbol: None,
                    }],
                    history: vec![],
                },
            }),
        );
        state.graph_index.nodes.push(NodeEntry {
            name: "store".into(),
            kind: NodeKind::Component,
            tags: vec![],
            hash: String::new(),
        });
        state.graph_index.nodes.push(NodeEntry {
            name: "atomic-writes".into(),
            kind: NodeKind::Decision,
            tags: vec![],
            hash: String::new(),
        });
        state.graph_index.edges.push(EdgeEntry {
            from: "atomic-writes".into(),
            to: "store".into(),
            kind: EdgeKind::BelongsTo,
        });
        state.rebuild_graph();

        let args = serde_json::json!({ "file": "./src/store" });
        let envelope = call_read_tool(&state, "get_decisions_for_file", &args);
        assert!(!envelope.is_error());
        let result: serde_json::Value = serde_json::from_str(envelope.text()).unwrap();
        assert_eq!(result["file"], "src/store", "leading ./ must be normalized");
        assert_eq!(result["count"], 1);
    }

    #[test]
    fn get_decisions_for_file_is_read_tool() {
        let list = tool_list();
        let tools = list["tools"].as_array().unwrap();
        let tool = tools
            .iter()
            .find(|t| t["name"] == "get_decisions_for_file")
            .unwrap();
        assert_eq!(tool["annotations"]["readOnlyHint"], true);
        assert_eq!(tool["inputSchema"]["required"][0], "file");
    }

    // ── C05: validate_consistency removed ─────────────────────────

    #[test]
    fn validate_consistency_absent_from_tool_list() {
        let list = tool_list();
        let tools = list["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(
            !names.contains(&"validate_consistency"),
            "validate_consistency should have been removed from the tool catalogue"
        );
    }

    #[test]
    fn dispatch_validate_consistency_returns_unknown_tool() {
        let state = empty_state();
        let envelope = call_read_tool(&state, "validate_consistency", &serde_json::json!({}));
        assert!(
            envelope.is_error(),
            "validate_consistency should be rejected as unknown"
        );
        assert!(
            envelope.text().contains("unknown tool"),
            "error should say 'unknown tool': {}",
            envelope.text(),
        );
    }

    // ── verify_against_decisions dispatch tests ─────────────────────

    /// State with an `mcp` component owning one decision that references
    /// `src/mcp/verify.rs`, an `mcp` decision that references an unrelated
    /// file, and one project-wide rule. Enough to exercise every branch of
    /// the response builder through the dispatch boundary.
    fn verify_state() -> ProjectState {
        use crate::store::Timestamp;
        use crate::store::schema::*;
        use std::sync::Arc;

        let ts = "2025-06-01T10:00:00Z".parse::<Timestamp>().unwrap();
        let mut state = empty_state();

        state.components.insert(
            "mcp".into(),
            Arc::new(ComponentFile {
                component: Component {
                    name: "mcp".into(),
                    description: "MCP server".into(),
                },
            }),
        );

        let decisions = [
            (
                "verify-tool",
                "mcp",
                vec!["src/mcp/verify.rs"],
                vec!["protocol"],
            ),
            (
                "watcher-debounce",
                "mcp",
                vec!["src/mcp/watcher.rs"],
                vec![],
            ),
            ("no-panic", "project", vec![], vec!["reliability"]),
        ];
        for (name, component, refs, tags) in decisions {
            state.decisions.insert(
                name.into(),
                Arc::new(DecisionFile {
                    decision: Decision {
                        component: component.into(),
                        choice: format!("choice for {name}"),
                        reason: format!("reason for {name}"),
                        alternatives: vec![],
                        tags: tags.iter().map(|t| (*t).into()).collect(),
                        attribution: Attribution::User,
                        created: ts,
                        code_refs: refs
                            .iter()
                            .map(|f| CodeRef {
                                file: (*f).into(),
                                symbol: None,
                            })
                            .collect(),
                        history: vec![],
                    },
                }),
            );
        }

        for (name, kind) in [
            ("mcp", NodeKind::Component),
            ("verify-tool", NodeKind::Decision),
            ("watcher-debounce", NodeKind::Decision),
            ("no-panic", NodeKind::Decision),
        ] {
            state.graph_index.nodes.push(NodeEntry {
                name: name.into(),
                kind,
                tags: vec![],
                hash: String::new(),
            });
        }
        for (from, to) in [
            ("verify-tool", "mcp"),
            ("watcher-debounce", "mcp"),
            ("no-panic", "project"),
        ] {
            state.graph_index.edges.push(EdgeEntry {
                from: from.into(),
                to: to.into(),
                kind: EdgeKind::BelongsTo,
            });
        }
        state.rebuild_graph();
        state
    }

    #[test]
    fn verify_against_decisions_missing_component() {
        let state = verify_state();
        let args = serde_json::json!({ "changed_files": ["src/mcp/verify.rs"] });
        let envelope = call_read_tool(&state, "verify_against_decisions", &args);
        assert!(envelope.is_error());
        assert!(envelope.text().contains("component"));
    }

    #[test]
    fn verify_against_decisions_missing_changed_files() {
        let state = verify_state();
        let args = serde_json::json!({ "component": "mcp" });
        let envelope = call_read_tool(&state, "verify_against_decisions", &args);
        assert!(envelope.is_error());
        assert!(envelope.text().contains("changed_files"));
    }

    #[test]
    fn verify_against_decisions_empty_changed_files() {
        let state = verify_state();
        let args = serde_json::json!({ "component": "mcp", "changed_files": [] });
        let envelope = call_read_tool(&state, "verify_against_decisions", &args);
        assert!(envelope.is_error(), "empty changed_files must be rejected");
    }

    #[test]
    fn verify_against_decisions_nonexistent_component() {
        let state = verify_state();
        let args = serde_json::json!({
            "component": "does-not-exist",
            "changed_files": ["src/mcp/verify.rs"],
        });
        let envelope = call_read_tool(&state, "verify_against_decisions", &args);
        assert!(envelope.is_error());
        assert!(
            envelope.text().contains("does not exist"),
            "error should say the component does not exist: {}",
            envelope.text()
        );
    }

    #[test]
    fn verify_against_decisions_invalid_path() {
        let state = verify_state();
        let args = serde_json::json!({
            "component": "mcp",
            "changed_files": ["/etc/passwd"],
        });
        let envelope = call_read_tool(&state, "verify_against_decisions", &args);
        assert!(envelope.is_error(), "absolute path must be rejected");
        assert!(
            envelope.text().contains("invalid file path"),
            "error should describe the issue: {}",
            envelope.text()
        );
    }

    #[test]
    fn verify_against_decisions_happy_path() {
        let state = verify_state();
        let args = serde_json::json!({
            "component": "mcp",
            "changed_files": ["src/mcp/verify.rs"],
        });
        let envelope = call_read_tool(&state, "verify_against_decisions", &args);
        assert!(!envelope.is_error(), "should succeed");

        let result: serde_json::Value = serde_json::from_str(envelope.text()).unwrap();
        assert_eq!(result["component"], "mcp");

        // Only verify-tool references the changed file; watcher-debounce does not.
        let to_verify = result["decisions_to_verify"].as_array().unwrap();
        assert_eq!(to_verify.len(), 1);
        assert_eq!(to_verify[0]["name"], "verify-tool");
        assert_eq!(to_verify[0]["affected_files"][0], "src/mcp/verify.rs");
        assert_eq!(to_verify[0]["code_refs"][0]["file"], "src/mcp/verify.rs");
        assert_eq!(to_verify[0]["tags"][0], "protocol");

        // watcher-debounce is the one unaffected component decision.
        assert_eq!(result["unaffected_count"], 1);

        // The project rule is always returned in full.
        let project = result["project_decisions"].as_array().unwrap();
        assert_eq!(project.len(), 1);
        assert_eq!(project[0]["name"], "no-panic");

        // Instructions carry the verdict vocabulary.
        let instructions = result["instructions"].as_str().unwrap();
        assert!(instructions.contains("RESPECTED"));
        assert!(instructions.contains("VIOLATED"));
        assert!(instructions.contains("NEEDS_REVIEW"));

        // A match means no "no decisions" message.
        assert!(result.get("message").is_none());
    }

    #[test]
    fn verify_against_decisions_no_match_returns_message() {
        let state = verify_state();
        let args = serde_json::json!({
            "component": "mcp",
            "changed_files": ["src/store/write.rs"],
        });
        let envelope = call_read_tool(&state, "verify_against_decisions", &args);
        assert!(!envelope.is_error());

        let result: serde_json::Value = serde_json::from_str(envelope.text()).unwrap();
        assert!(result["decisions_to_verify"].as_array().unwrap().is_empty());
        assert_eq!(result["message"], "No decisions affected by these changes");
        // Both mcp decisions filtered out.
        assert_eq!(result["unaffected_count"], 2);
        // Project decisions still included.
        assert_eq!(result["project_decisions"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn verify_against_decisions_is_read_tool() {
        let list = tool_list();
        let tools = list["tools"].as_array().unwrap();
        let tool = tools
            .iter()
            .find(|t| t["name"] == "verify_against_decisions")
            .unwrap();
        assert_eq!(tool["annotations"]["readOnlyHint"], true);
        assert!(!is_write_tool("verify_against_decisions"));
        let required = tool["inputSchema"]["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "component"));
        assert!(required.iter().any(|v| v == "changed_files"));
    }

    fn empty_state() -> ProjectState {
        crate::store::testing::empty_project_state()
    }
}
