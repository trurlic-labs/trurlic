//! The tools that predate specs: their `tools/list` entries written out by
//! hand, and the parsing of their arguments. Both are reads.

use std::sync::LazyLock;

use serde_json::{Value, json};

use super::context;
use super::result::{ToolEnvelope, refusal, tool_result};
use super::spec;
use crate::store::ProjectState;

/// The `tools/list` result: every spec-built tool, then the tools that
/// predate specs. Built once on first access, returned by reference.
static TOOL_DEFINITIONS: LazyLock<Value> = LazyLock::new(|| {
    let mut tools: Vec<Value> = spec::TOOLS.iter().map(|tool| tool.definition()).collect();
    tools.extend(unspecified_tools());
    json!({ "tools": tools })
});

/// The annotations of a tool that reads the graph and nothing else.
fn read_only(title: &str) -> Value {
    json!({
        "title": title,
        "readOnlyHint": true,
        "destructiveHint": false,
        "idempotentHint": true,
        "openWorldHint": false
    })
}

/// The tools whose schemas are written out by hand, each parsing its own
/// arguments below.
fn unspecified_tools() -> [Value; 2] {
    [
        get_context_definition(),
        get_decisions_for_file_definition(),
    ]
}

fn get_context_definition() -> Value {
    json!({
        "name": "get_context",
        "description": "Get the architectural context for a component: its decisions, \
            the project-wide rules, the decisions of connected components and of \
            what its decisions depend on, and an authoritative brief for coding \
            agents. Use depth=\"constraints\" for a compact brief of choices only.",
        "annotations": read_only("Get context"),
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
                        decisions."
                }
            },
            "required": ["component"]
        }
    })
}

fn get_decisions_for_file_definition() -> Value {
    json!({
        "name": "get_decisions_for_file",
        "description": "Reverse lookup: given a file or directory path, find every \
            decision anchored there: on the file itself, on a file under the \
            directory, or on a directory that holds the file. Returns decisions \
            sorted by component then name, each with its matching anchors. \
            Project-wide rules apply everywhere and are not listed unless anchored \
            there; get_context(\"project\") lists them.",
        "annotations": read_only("Get decisions for file"),
        "inputSchema": {
            "type": "object",
            "properties": {
                "file": {
                    "type": "string",
                    "description": "File or directory path relative to the project \
                        root (e.g. 'src/store/load.rs' or 'src/store'). A leading './' \
                        is dropped; backslashes, '..' and absolute paths are refused."
                }
            },
            "required": ["file"]
        }
    })
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

/// Call the tool `name` on `state`, the graph loaded under the shared lock.
pub(crate) fn call_read_tool(state: &ProjectState, name: &str, args: &Value) -> ToolEnvelope {
    let answer = match name {
        "get_context" => get_context(state, args),
        "get_decisions_for_file" => get_decisions_for_file(state, args),
        _ => Err(format!("unknown tool: {name}")),
    };
    match answer {
        Ok(payload) => tool_result(&payload),
        Err(message) => refusal(&message),
    }
}

fn get_context(state: &ProjectState, args: &Value) -> Result<Value, String> {
    let component = args
        .get("component")
        .and_then(Value::as_str)
        .ok_or("missing required parameter: component")?;
    let task = args.get("task").and_then(Value::as_str);
    let depth = match args.get("depth").and_then(Value::as_str) {
        Some("constraints") => context::ContextDepth::Constraints,
        _ => context::ContextDepth::Full,
    };
    context::get_context(state, component, task, depth)
}

fn get_decisions_for_file(state: &ProjectState, args: &Value) -> Result<Value, String> {
    let file = args
        .get("file")
        .and_then(Value::as_str)
        .ok_or("missing required parameter: file")?;
    context::get_decisions_for_file(state, file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testing::{anchored, empty_state};

    fn tools() -> &'static [Value] {
        tool_list()["tools"].as_array().unwrap()
    }

    /// The tools that predate specs, listed after the spec-built ones.
    const UNSPECIFIED: [&str; 2] = ["get_context", "get_decisions_for_file"];

    /// `tools/list` is the specs, in registry order, as each defines
    /// itself, then the two reads written out by hand.
    #[test]
    fn tool_list_is_the_specs_then_the_two_reads() {
        let (specified, unspecified) = tools().split_at(spec::TOOLS.len());
        for (listed, tool) in specified.iter().zip(spec::TOOLS) {
            assert_eq!(*listed, tool.definition(), "{}", tool.name());
            assert!(listed["outputSchema"].is_object(), "{}", tool.name());
        }
        let names: Vec<&str> = unspecified
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, UNSPECIFIED);
        for tool in unspecified {
            assert_eq!(
                tool["annotations"]["readOnlyHint"], true,
                "{}",
                tool["name"]
            );
            assert_eq!(tool["inputSchema"]["type"], "object");
        }
        for tool in tools() {
            assert_eq!(
                tool["annotations"]["openWorldHint"], false,
                "{}",
                tool["name"]
            );
        }
    }

    /// Claude Code cuts a tool description at 4,096 characters; half of
    /// that keeps a session's fixed cost small.
    #[test]
    fn every_listed_description_fits_its_surface_budget() {
        const MAX_DESCRIPTION_CHARS: usize = 2048;
        for tool in tools() {
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
        for tool in tools() {
            assert!(is_listed(tool["name"].as_str().unwrap()));
        }
        for gone in ["record_decision", "check_pattern", "get_architecture", ""] {
            assert!(!is_listed(gone), "{gone}");
        }
    }

    #[test]
    fn a_missing_argument_or_an_unknown_tool_is_refused() {
        let state = empty_state();
        for (tool, needed) in [
            ("get_context", "component"),
            ("get_decisions_for_file", "file"),
        ] {
            let envelope = call_read_tool(&state, tool, &json!({}));
            assert!(envelope.is_error(), "{tool}");
            assert!(
                envelope.text().contains(needed),
                "{tool}: {}",
                envelope.text()
            );
        }
        let envelope = call_read_tool(&state, "nonexistent", &json!({}));
        assert!(envelope.text().contains("unknown tool"));
    }

    #[test]
    fn get_decisions_for_file_answers_with_the_anchored_decisions() {
        let mut state = empty_state();
        state.decisions.insert(
            "atomic".into(),
            anchored("project", &["src/store/commit.rs"]),
        );
        let envelope = call_read_tool(
            &state,
            "get_decisions_for_file",
            &json!({ "file": "src/store" }),
        );
        assert!(!envelope.is_error(), "{}", envelope.text());
        let payload: Value = serde_json::from_str(envelope.text()).unwrap();
        assert_eq!(payload["decisions"][0]["name"], "atomic");

        let refused = call_read_tool(&state, "get_decisions_for_file", &json!({ "file": "../x" }));
        assert!(refused.is_error());
    }
}
