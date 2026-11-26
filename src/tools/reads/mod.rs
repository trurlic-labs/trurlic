//! The reads whose listing and arguments are written by hand, without a
//! spec: their schemas, and the parsing of their arguments. [`context`]
//! assembles what they answer.

use std::path::Path;

use serde_json::{Map, Value, json};

mod context;
use crate::discovery::discover;
use crate::store::ProjectState;
use crate::tools::result::{ToolError, ToolResult};

/// A read whose listing and arguments are written by hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Read {
    /// `get_context`: a component's context and its brief.
    Context,
    /// `get_decisions_for_file`: the decisions anchored on a path.
    DecisionsForFile,
}

const CONTEXT_DESCRIPTION: &str = "Get the architectural context for a component: its \
    decisions, the project-wide rules, the decisions of connected components and of what its \
    decisions depend on, and an authoritative brief for coding agents. Use \
    depth=\"constraints\" for a compact brief of choices only.";

const DECISIONS_FOR_FILE_DESCRIPTION: &str = "Reverse lookup: given a file or directory path, \
    find every decision anchored there: on the file itself, on a file under the directory, or \
    on a directory that holds the file. Returns decisions sorted by component then name, each \
    with its matching anchors. Project-wide rules apply everywhere and are not listed unless \
    anchored there; get_context(\"project\") lists them.";

impl Read {
    pub(super) const fn name(self) -> &'static str {
        match self {
            Self::Context => "get_context",
            Self::DecisionsForFile => "get_decisions_for_file",
        }
    }

    pub(super) const fn title(self) -> &'static str {
        match self {
            Self::Context => "Get context",
            Self::DecisionsForFile => "Get decisions for file",
        }
    }

    pub(super) const fn description(self) -> &'static str {
        match self {
            Self::Context => CONTEXT_DESCRIPTION,
            Self::DecisionsForFile => DECISIONS_FOR_FILE_DESCRIPTION,
        }
    }

    /// The JSON Schema of the read's arguments.
    pub(super) fn input_schema(self) -> Value {
        match self {
            Self::Context => json!({
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
            }),
            Self::DecisionsForFile => json!({
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
            }),
        }
    }

    /// Call the read with `arguments` on the graph of the store found from
    /// `cwd`, loaded under the shared lock; a failed load is a refusal.
    pub(super) fn call(self, cwd: &Path, arguments: &Map<String, Value>) -> ToolResult {
        let state = match discover(cwd).and_then(|store| store.load_state()) {
            Ok(state) => state,
            Err(e) => return ToolResult::refusal(&e.into()),
        };
        match self.answer(&state, arguments) {
            Ok(payload) => ToolResult::unstructured(&payload),
            Err(message) => ToolResult::refusal(&ToolError::refused(message)),
        }
    }

    /// The read's payload on `state`, or why its arguments are refused.
    fn answer(self, state: &ProjectState, arguments: &Map<String, Value>) -> Result<Value, String> {
        match self {
            Self::Context => get_context(state, arguments),
            Self::DecisionsForFile => get_decisions_for_file(state, arguments),
        }
    }
}

fn get_context(state: &ProjectState, args: &Map<String, Value>) -> Result<Value, String> {
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

fn get_decisions_for_file(
    state: &ProjectState,
    args: &Map<String, Value>,
) -> Result<Value, String> {
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
    use crate::store::{Placement, Store};
    use crate::tools::ResultKind;

    fn arguments(value: &Value) -> &Map<String, Value> {
        value.as_object().unwrap()
    }

    /// A call the read refuses is a refusal of kind `refused` whose message
    /// names what is wrong: a missing argument, a path out of the project,
    /// a component the graph does not hold.
    #[test]
    fn a_refused_call_is_a_refusal_naming_its_fault() {
        let tmp = tempfile::TempDir::new().unwrap();
        Store::create(tmp.path(), "demo", Placement::Tracked).unwrap();
        let cases = [
            (Read::Context, json!({}), "component"),
            (Read::DecisionsForFile, json!({}), "file"),
            (
                Read::DecisionsForFile,
                json!({ "file": "../x" }),
                "invalid file path",
            ),
            (
                Read::Context,
                json!({ "component": "nope" }),
                "`nope` does not exist",
            ),
        ];
        for (read, given, named) in cases {
            let (text, kind) = read.call(tmp.path(), arguments(&given)).into_parts();
            assert_eq!(kind, ResultKind::Refusal, "{}: {text}", read.name());
            let refusal: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(refusal["error"]["kind"], "refused", "{text}");
            let message = refusal["error"]["message"].as_str().unwrap();
            assert!(message.contains(named), "{}: {message}", read.name());
        }
    }

    #[test]
    fn get_decisions_for_file_answers_with_the_anchored_decisions() {
        let mut state = empty_state();
        state.decisions.insert(
            "atomic".into(),
            anchored("project", &["src/store/commit.rs"]),
        );
        let payload = Read::DecisionsForFile
            .answer(&state, arguments(&json!({ "file": "src/store" })))
            .unwrap();
        assert_eq!(payload["decisions"][0]["name"], "atomic");
    }
}
