//! Every tool result fits 24 KiB on a graph whose whole answers exceed it,
//! and every payload stays JSON the agent can parse.

use serde_json::{Value, json};

use crate::harness::{McpClient, Project, decision};

const MAX_TOOL_RESULT_BYTES: usize = 24 * 1024;

/// Decisions with reasons long enough that `auth`'s context and file queries
/// each exceed the budget several times over.
const DECISIONS: usize = 60;

fn build_graph(project: &Project) {
    project.write(
        "components/auth.toml",
        "description = \"\"\nconnects_to = [\"storage\"]\n",
    );
    project.write("components/storage.toml", "description = \"\"\n");
    let reason = "Each hop verifies who sent the request. ".repeat(25);
    for i in 0..DECISIONS {
        let choice = format!("Sign request kind {i:02} with a rotating key");
        let extra = format!(
            "tags = [\"security\"]\nanchors = [{{ file = \"src/auth.rs\", symbol = \"sign_{i}\" }}]\n"
        );
        let text = decision("auth", &choice, &extra)
            .replace("Recorded so the test has a decision to read", &reason);
        project.write(&format!("decisions/sign-{i:02}.toml"), &text);
    }
    let rule = decision("project", "Every request is signed", "");
    project.write("decisions/signed.toml", &rule);
}

fn fitted(client: &mut McpClient, tool: &str, arguments: Value) -> Value {
    let text = client.call_tool_text(tool, arguments.clone());
    assert!(
        text.len() <= MAX_TOOL_RESULT_BYTES,
        "{tool} {arguments}: {} bytes",
        text.len()
    );
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{tool} {arguments}: {e}"))
}

#[test]
fn every_tool_result_fits_the_budget() {
    let project = Project::init();
    build_graph(&project);
    let (mut client, _) = McpClient::connect(&project);

    for (tool, arguments) in [
        ("get_context", json!({ "component": "auth" })),
        ("get_decisions_for_file", json!({ "file": "src/auth.rs" })),
    ] {
        let payload = fitted(&mut client, tool, arguments);
        assert!(
            payload["truncated"].is_array(),
            "{tool} must exceed the budget"
        );
    }
    for arguments in [
        json!({ "component": "auth", "depth": "constraints" }),
        json!({ "component": "project" }),
    ] {
        fitted(&mut client, "get_context", arguments);
    }
}
