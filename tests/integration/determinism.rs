//! A read tool answers with the same bytes in every process. Each process
//! seeds its hash collections differently, so a hash-ordered list in a
//! response shows up as two orders. One server builds the graph and reads
//! from the state its writes left in memory; a second server reads the same
//! graph loaded from disk.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use crate::harness::{McpClient, Project};

/// The hub every other component connects to or from, so its context and
/// step prompts carry the decisions of six neighbours.
const HUB: &str = "auth";
const DOWNSTREAM: [&str; 4] = ["billing", "mail", "search", "storage"];
const UPSTREAM: [&str; 2] = ["audit", "gateway"];

/// Two decisions per component, both anchored in the component's source
/// file, so file queries, verdicts and patterns span several components.
fn build_graph(writer: &mut McpClient) {
    let neighbours = DOWNSTREAM.iter().chain(&UPSTREAM);
    for component in std::iter::once(&HUB).chain(neighbours.clone()) {
        writer.call_tool("add_component", json!({ "name": component }));
    }
    for to in DOWNSTREAM {
        writer.call_tool("add_connection", json!({ "from": HUB, "to": to }));
    }
    for from in UPSTREAM {
        writer.call_tool("add_connection", json!({ "from": from, "to": HUB }));
    }

    let mut keyed = Vec::new();
    for component in std::iter::once(&HUB).chain(neighbours) {
        keyed.push(decide(writer, component, "Sign every request key"));
        decide(writer, component, "Retry failed calls with backoff");
    }
    writer.call_tool(
        "record_pattern",
        json!({
            "name": "Signed requests",
            "description": "Every component signs what it sends",
            "decisions": keyed,
        }),
    );
    // The writer's last call is a write, so its reads see the state that
    // write left in memory.
    writer.call_tool(
        "update_decision",
        json!({ "name": keyed[0], "mode": "revise", "tags": ["security", "keys"] }),
    );
}

fn decide(writer: &mut McpClient, component: &str, choice: &str) -> String {
    let recorded = writer.call_tool(
        "record_decision",
        json!({
            "component": component,
            "choice": format!("{component}: {choice}"),
            "reason": "Keeps the component's traffic verifiable and bounded",
            "attribution": "agent",
            "tags": ["security"],
            "code_refs": [{ "file": format!("src/{component}.rs") }],
        }),
    );
    recorded["name"].as_str().unwrap().to_owned()
}

/// One call per read tool and argument shape that reaches a different
/// assembly path.
fn read_calls() -> Vec<(&'static str, Value)> {
    let mut calls = vec![
        ("get_architecture", json!({})),
        ("get_context", json!({ "component": HUB })),
        (
            "get_context",
            json!({ "component": HUB, "depth": "constraints" }),
        ),
        ("get_context", json!({ "component": "project" })),
        (
            "check_pattern",
            json!({ "description": "sign requests with a rotating key" }),
        ),
        ("get_decisions_for_file", json!({ "file": "src" })),
        (
            "get_decision_history",
            json!({ "name": "auth-sign-every-request-key" }),
        ),
        (
            "verify_against_decisions",
            json!({ "component": HUB, "changed_files": ["src"] }),
        ),
    ];
    for task_type in ["feature", "review", "harden"] {
        calls.push((
            "advance",
            json!({ "component": HUB, "task_type": task_type, "mode": "agent" }),
        ));
    }
    for step in [
        "verify_constraints",
        "impact_check",
        "pattern_detection",
        "drift_check",
        "coverage_audit",
    ] {
        calls.push((
            "get_step_prompt",
            json!({ "component": HUB, "step": step, "mode": "agent" }),
        ));
    }
    calls
}

fn read_only_tools(client: &mut McpClient) -> BTreeSet<String> {
    client.request("tools/list", json!({}))["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|tool| tool["annotations"]["readOnlyHint"] == true)
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn read_tools_answer_the_same_bytes_in_a_writer_and_a_fresh_server() {
    let project = Project::init();
    let (mut writer, _) = McpClient::connect(&project);
    build_graph(&mut writer);
    let (mut reader, _) = McpClient::connect(&project);

    let calls = read_calls();
    let called: BTreeSet<String> = calls.iter().map(|(tool, _)| (*tool).to_owned()).collect();
    assert_eq!(called, read_only_tools(&mut reader), "read tools called");

    for (tool, arguments) in calls {
        let written = writer.call_tool_text(tool, arguments.clone());
        let loaded = reader.call_tool_text(tool, arguments.clone());
        assert!(
            written == loaded,
            "{tool} {arguments} differs between processes\n\
             writer: {written}\nreader: {loaded}"
        );
    }
}
