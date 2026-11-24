//! Every tool result fits 24 KiB on a graph whose whole answers exceed it,
//! and every payload stays JSON the agent can parse.

use serde_json::{Value, json};

use crate::harness::{McpClient, Project, decision};

const MAX_TOOL_RESULT_BYTES: usize = 24 * 1024;

/// What the `lint` section says of the findings it leaves out: they rank
/// at or below the last listed, ties included, and no surface lists them.
const LINT_MORE: &str = "the section lists the best pairs; the ones left out rank at or below \
                         the last listed, and come into view as the listed ones are settled \
                         (merged, or revised apart)";

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

/// A graph whose issues alone exceed the budget several times over, and
/// whose components fill a good part of it: each status section fits, the
/// issues section counts what it leaves out and says where the rest is,
/// and asked for alone it shows more of them; so does the lint section,
/// whose pairs grow with the square of the decisions. Nothing is cut
/// silently: no result carries a `truncated` notice.
#[test]
fn status_fits_the_budget_and_counts_what_it_leaves_out() {
    const BROKEN: usize = 400;
    const COMPONENTS: usize = 300;
    let project = Project::init();
    for i in 0..COMPONENTS {
        project.write(
            &format!("components/part-{i:03}.toml"),
            "description = \"\"\n",
        );
    }
    for i in 0..BROKEN {
        let text = decision("project", &format!("Rule {i:03}"), "")
            .replace("Recorded so the test has a decision to read", "short");
        project.write(&format!("decisions/rule-{i:03}.toml"), &text);
    }
    let (mut client, _) = McpClient::connect(&project);

    let whole = fitted(&mut client, "status", json!({}));
    let alone = fitted(&mut client, "status", json!({ "section": "issues" }));

    for status in [&whole, &alone] {
        assert!(status.get("truncated").is_none(), "{status}");
        let issues = &status["issues"];
        assert_eq!(issues["total"], BROKEN);
        let shown = issues["items"].as_array().unwrap().len();
        assert_eq!(issues["omitted"], BROKEN - shown);
        assert_eq!(issues["more"], "`trurlic check` lists every issue");
    }
    let shown = |status: &Value| status["issues"]["items"].as_array().unwrap().len();
    assert!(
        shown(&alone) > shown(&whole),
        "{} {}",
        shown(&alone),
        shown(&whole)
    );
    assert!(alone.get("components").is_none());
    assert_eq!(
        whole["components"]["items"][0],
        json!({ "name": "project", "decisions": BROKEN, "proposed": 0 })
    );

    // The decisions' texts differ only in digits, which no term holds, so
    // every pair is a likely duplicate at a score of one.
    let lint_alone = fitted(&mut client, "status", json!({ "section": "lint" }));
    let pairs = BROKEN * (BROKEN - 1) / 2;
    for status in [&whole, &lint_alone] {
        let lint = &status["lint"];
        assert_eq!(lint["total"], pairs);
        let shown = lint["items"].as_array().unwrap().len();
        assert_eq!(lint["omitted"], pairs - shown);
        assert_eq!(lint["more"], LINT_MORE);
        // The pairs left out tie with the last listed one.
        assert_eq!(lint["items"][shown - 1]["score"], "1.000000");
        assert_eq!(
            lint["items"][0],
            json!({
                "kind": "duplicate", "decision": "rule-000", "other": "rule-001",
                "score": "1.000000",
            })
        );
    }
    let shown = |status: &Value| status["lint"]["items"].as_array().unwrap().len();
    assert!(shown(&lint_alone) > shown(&whole));
}
