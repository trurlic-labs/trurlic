//! A read tool answers with the same bytes in every process. Each process
//! has its own thread pool, scheduling and allocator state, so an answer
//! that depended on the order the parallel load met the files would show
//! up as two answers.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use crate::harness::{McpClient, Project, decision};

/// The hub every other component connects to or from, so its context
/// carries the decisions of six neighbours.
const HUB: &str = "auth";
const DOWNSTREAM: [&str; 4] = ["billing", "mail", "search", "storage"];
const UPSTREAM: [&str; 2] = ["audit", "gateway"];

/// Two decisions per component, both anchored in the component's source
/// file, the second depending on the first, so file queries and
/// dependencies span several components.
fn build_graph(project: &Project) {
    let downstream: Vec<String> = DOWNSTREAM.iter().map(|to| format!("\"{to}\"")).collect();
    project.write(
        &format!("components/{HUB}.toml"),
        &format!(
            "description = \"\"\nconnects_to = [{}]\n",
            downstream.join(", ")
        ),
    );
    for name in DOWNSTREAM {
        project.write(&format!("components/{name}.toml"), "description = \"\"\n");
    }
    for name in UPSTREAM {
        project.write(
            &format!("components/{name}.toml"),
            &format!("description = \"\"\nconnects_to = [\"{HUB}\"]\n"),
        );
    }
    for component in std::iter::once(&HUB).chain(&DOWNSTREAM).chain(&UPSTREAM) {
        let anchor = format!("anchors = [{{ file = \"src/{component}.rs\" }}]\n");
        project.write(
            &format!("decisions/{component}-sign.toml"),
            &decision(
                component,
                &format!("{component}: sign every request"),
                &anchor,
            ),
        );
        let extra = format!("{anchor}depends_on = [\"{component}-sign\"]\nstatus = \"proposed\"\n");
        let retry = decision(
            component,
            &format!("{component}: retry with backoff"),
            &extra,
        )
        .replace("status = \"accepted\"\n", "");
        project.write(&format!("decisions/{component}-retry.toml"), &retry);
    }
    project.write(
        "decisions/rule.toml",
        &decision("project", "Every call is bounded", ""),
    );
}

/// One call per read tool and argument shape that reaches a different
/// assembly path.
fn read_calls() -> Vec<(&'static str, Value)> {
    vec![
        ("get_context", json!({ "component": HUB })),
        (
            "get_context",
            json!({ "component": HUB, "depth": "constraints" }),
        ),
        ("get_context", json!({ "component": "project" })),
        ("get_decisions_for_file", json!({ "file": "src" })),
    ]
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
fn read_tools_answer_the_same_bytes_in_two_servers() {
    let project = Project::init();
    build_graph(&project);
    project.run_ok(&["check"]);
    let (mut first, _) = McpClient::connect(&project);
    let (mut second, _) = McpClient::connect(&project);

    let calls = read_calls();
    let called: BTreeSet<String> = calls.iter().map(|(tool, _)| (*tool).to_owned()).collect();
    assert_eq!(called, read_only_tools(&mut second), "read tools called");

    for (tool, arguments) in calls {
        let one = first.call_tool_text(tool, arguments.clone());
        let two = second.call_tool_text(tool, arguments.clone());
        assert!(
            one == two,
            "{tool} {arguments} differs between processes\nfirst: {one}\nsecond: {two}"
        );
    }
}
