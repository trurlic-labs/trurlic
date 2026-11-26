//! The `tools/list` result: every tool of the registry as MCP lists it,
//! built once on first access and returned by reference. A tool built on
//! a spec lists the schemas its spec walks; a read that predates specs,
//! the schema it writes out by hand.

use std::sync::LazyLock;

use serde_json::{Value, json};

use crate::tools::{Destructive, TOOLS, Tool};

static TOOL_LIST: LazyLock<Value> = LazyLock::new(|| {
    let tools: Vec<Value> = TOOLS.iter().map(|&tool| entry(tool)).collect();
    json!({ "tools": tools })
});

pub(super) fn tool_list() -> &'static Value {
    &TOOL_LIST
}

/// `tool`'s entry in `tools/list`.
fn entry(tool: Tool) -> Value {
    match tool {
        Tool::Spec(spec) => json!({
            "name": spec.name(),
            "title": spec.title(),
            "description": spec.description(),
            "annotations": annotations(spec.title(), spec.is_read_only(), spec.destructive()),
            "inputSchema": spec.input_schema(),
            "outputSchema": spec.output_schema(),
        }),
        Tool::Legacy(read) => json!({
            "name": read.name(),
            "description": read.description(),
            "annotations": annotations(read.title(), true, Destructive::No),
            "inputSchema": read.input_schema(),
        }),
    }
}

/// MCP's hints of a tool: a read is idempotent, and no tool reaches past
/// the project.
fn annotations(title: &str, read_only: bool, destructive: Destructive) -> Value {
    json!({
        "title": title,
        "readOnlyHint": read_only,
        "destructiveHint": destructive == Destructive::Yes,
        "idempotentHint": read_only,
        "openWorldHint": false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listed() -> &'static [Value] {
        tool_list()["tools"].as_array().unwrap()
    }

    /// `tools/list` is the registry in its order, each spec-built tool
    /// with the facts and schemas its spec gives, each read that predates
    /// specs a read.
    #[test]
    fn tool_list_is_the_registry_as_each_tool_declares_itself() {
        assert_eq!(listed().len(), TOOLS.len());
        for (entry, tool) in listed().iter().zip(TOOLS) {
            assert_eq!(entry["name"], tool.name());
            let hints = &entry["annotations"];
            assert_eq!(hints["openWorldHint"], false, "{entry}");
            assert_eq!(hints["idempotentHint"], hints["readOnlyHint"], "{entry}");
            match tool {
                Tool::Spec(spec) => {
                    assert_eq!(hints["readOnlyHint"], spec.is_read_only(), "{entry}");
                    let destructive = spec.destructive() == Destructive::Yes;
                    assert_eq!(hints["destructiveHint"], destructive, "{entry}");
                    assert_eq!(entry["inputSchema"], Value::Object(spec.input_schema()));
                    assert_eq!(entry["outputSchema"], Value::Object(spec.output_schema()));
                }
                Tool::Legacy(read) => {
                    assert_eq!(hints["readOnlyHint"], true, "{entry}");
                    assert_eq!(entry["inputSchema"], read.input_schema());
                }
            }
        }
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
