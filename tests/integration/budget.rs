//! Every tool result fits 24 KiB on a graph whose whole answers exceed it,
//! and every payload stays JSON the agent can parse.

use serde_json::{Value, json};

use crate::harness::{McpClient, Project};

const MAX_TOOL_RESULT_BYTES: usize = 24 * 1024;

/// Decisions with reasons long enough that `auth`'s context, step prompts
/// and file queries each exceed the budget several times over.
const DECISIONS: usize = 60;

fn build_graph(client: &mut McpClient) -> Vec<String> {
    client.call_tool("add_component", json!({ "name": "auth" }));
    client.call_tool("add_component", json!({ "name": "storage" }));
    client.call_tool("add_connection", json!({ "from": "auth", "to": "storage" }));
    (0..DECISIONS)
        .map(|i| {
            let recorded = client.call_tool(
                "record_decision",
                json!({
                    "component": "auth",
                    "choice": format!("Sign request kind {i:02} with a rotating key"),
                    "reason": "Each hop verifies who sent the request. ".repeat(25),
                    "attribution": "agent",
                    "tags": ["security"],
                    "code_refs": [{ "file": "src/auth.rs", "symbol": format!("sign_{i}") }],
                }),
            );
            recorded["name"].as_str().unwrap().to_owned()
        })
        .collect()
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
    let (mut client, _) = McpClient::connect(&project);
    let names = build_graph(&mut client);

    let context = fitted(&mut client, "get_context", json!({ "component": "auth" }));
    assert!(
        context["truncated"].is_array(),
        "the graph must exceed the budget"
    );
    let walk = fitted(
        &mut client,
        "get_step_prompt",
        json!({ "component": "auth", "step": "walk_decisions", "mode": "interactive" }),
    );
    let instructions = walk["system_instructions"].as_str().unwrap();
    assert!(instructions.contains("decisions omitted to fit"), "{walk}");
    assert!(instructions.contains("INTERACTION PROTOCOL"), "{walk}");

    let reads = [
        (
            "get_context",
            json!({ "component": "auth", "depth": "constraints" }),
        ),
        ("get_architecture", json!({})),
        (
            "check_pattern",
            json!({ "description": "sign request rotating key" }),
        ),
        ("get_decisions_for_file", json!({ "file": "src/auth.rs" })),
        ("get_decision_history", json!({ "name": names[0] })),
        (
            "verify_against_decisions",
            json!({ "component": "auth", "changed_files": ["src"] }),
        ),
        (
            "advance",
            json!({ "component": "auth", "task_type": "review", "mode": "agent" }),
        ),
    ];
    for (tool, arguments) in reads {
        fitted(&mut client, tool, arguments);
    }
    for step in [
        "verify_constraints",
        "drift_check",
        "cover_concerns",
        "pattern_detection",
    ] {
        for mode in ["agent", "interactive"] {
            let arguments = json!({ "component": "auth", "step": step, "mode": mode });
            let prompt = fitted(&mut client, "get_step_prompt", arguments);
            assert!(prompt.get("truncated").is_none(), "{step} {mode}: {prompt}");
        }
    }

    let writes = [
        (
            "record_pattern",
            json!({ "name": "Signed requests", "description": "Every hop signs", "decisions": names }),
        ),
        (
            "update_decision",
            json!({ "name": names[1], "mode": "revise", "tags": ["security", "keys"] }),
        ),
        ("remove_pattern", json!({ "name": "signed-requests" })),
        ("remove_decision", json!({ "name": names[2] })),
    ];
    for (tool, arguments) in writes {
        fitted(&mut client, tool, arguments);
    }
}
