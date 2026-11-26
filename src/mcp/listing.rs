//! The `tools/list` result: every tool of the registry as MCP lists it,
//! built once on first access and returned by reference. An entry names
//! in MCP's words what the tool states of itself.

use std::sync::LazyLock;

use serde_json::{Map, Value, json};

use crate::tools::{Destructive, TOOLS, Tool};

static TOOL_LIST: LazyLock<Value> = LazyLock::new(|| {
    let tools: Vec<Value> = TOOLS.iter().map(|&tool| entry(tool)).collect();
    json!({ "tools": tools })
});

pub(super) fn tool_list() -> &'static Value {
    &TOOL_LIST
}

/// `tool`'s entry in `tools/list`. A tool with an output schema names its
/// title beside its name as well; one without names it among its hints
/// alone.
fn entry(tool: Tool) -> Value {
    let mut entry = Map::from_iter([
        ("name".to_owned(), json!(tool.name())),
        ("description".to_owned(), json!(tool.description())),
        ("annotations".to_owned(), annotations(tool)),
        ("inputSchema".to_owned(), tool.input_schema()),
    ]);
    if let Some(output) = tool.output_schema() {
        entry.insert("title".to_owned(), json!(tool.title()));
        entry.insert("outputSchema".to_owned(), output);
    }
    Value::Object(entry)
}

/// MCP's hints of a tool: a read is idempotent, and no tool reaches past
/// the project.
fn annotations(tool: Tool) -> Value {
    json!({
        "title": tool.title(),
        "readOnlyHint": tool.is_read_only(),
        "destructiveHint": tool.destructive() == Destructive::Yes,
        "idempotentHint": tool.is_read_only(),
        "openWorldHint": false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listed() -> &'static [Value] {
        tool_list()["tools"].as_array().unwrap()
    }

    /// Claude Code cuts a tool description at 4,096 characters; half of
    /// that keeps a session's fixed cost small.
    #[test]
    fn every_listed_description_fits_its_surface_budget() {
        const MAX_DESCRIPTION_CHARS: usize = 2048;
        for tool in listed() {
            let description = tool["description"].as_str().unwrap();
            assert!(
                description.chars().count() <= MAX_DESCRIPTION_CHARS,
                "{}: {} chars",
                tool["name"],
                description.chars().count()
            );
        }
    }
}
