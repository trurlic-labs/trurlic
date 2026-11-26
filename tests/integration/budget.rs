//! Every tool result fits 24 KiB on a graph whose whole answers exceed it,
//! and every payload stays JSON the agent can parse.

use serde_json::{Value, json};

use crate::harness::{McpClient, Project, decision};

const MAX_TOOL_RESULT_BYTES: usize = 24 * 1024;

/// What the `lint` section says of the findings it leaves out: a page of
/// decisions by name, read on from `next`.
const LINT_MORE: &str = "each kind lists what fits of the page and `counts.lint` \
     counts it: the duplicate pairs and the quality findings of the decisions before the \
     `next` that `counts.lint.pairs` or `counts.lint.patterns` names, which `section: \
     \"lint\"` with `from` lists on from, a pair on the page of the later-named of its \
     decisions the pass probed; the decisions with only file anchors left out follow the \
     last listed by name; the listed ones settled (pairs merged or revised apart, decisions \
     brought to the bar or anchored to a symbol), the rest come into view";

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
    // every pair is a likely duplicate at a score of one; and each lacks an
    // alternative and an anchor, two quality findings. The pairs come a
    // page at a time, each page ending where the budget cut its pairs.
    let lint_alone = fitted(&mut client, "status", json!({ "section": "lint" }));
    let (pairs, shortfalls) = (BROKEN * (BROKEN - 1) / 2, 2 * BROKEN);
    for status in [&whole, &lint_alone] {
        let lint = &status["lint"];
        let counts = &status["counts"]["lint"];
        let paged = counts["duplicates"].as_u64().unwrap() as usize;
        assert!(paged > 0 && paged < pairs, "{counts}");
        assert_eq!(counts["pairs"]["why"], "page", "{counts}");
        assert_eq!(counts["quality"], shortfalls, "{counts}");
        let items = lint["items"].as_array().unwrap();
        let listed = lint["total"].as_u64().unwrap() as usize;
        assert_eq!(lint["omitted"], listed - items.len());
        assert_eq!(lint["more"], LINT_MORE);
        let of = |kind: &str| items.iter().filter(|item| item["kind"] == kind).count();
        // Each kind keeps its share of the budget: the pairs, far more,
        // crowd out none of the quality findings' room. A `lint` call ends
        // the quality findings where the pairs' page ends.
        let (listed_pairs, listed_shortfalls) = (of("duplicate"), of("quality"));
        assert!(listed_shortfalls > 0 && listed_pairs > 0);
        if status == &whole {
            assert!(
                listed_shortfalls * 3 > listed_pairs,
                "{listed_pairs} {listed_shortfalls}"
            );
        }
        assert_eq!(
            items[0],
            json!({
                "kind": "duplicate", "decision": "rule-000", "other": "rule-001",
                "score": "1.000000",
            })
        );
        assert_eq!(
            items[listed_pairs],
            json!({
                "kind": "quality", "decision": "rule-000", "field": "alternatives",
                "rule": "alternative_required", "level": "refuse",
                "limit": { "measured": 0, "bound": 1, "unit": "alternatives" },
            })
        );
    }
    let shown = |status: &Value| status["lint"]["items"].as_array().unwrap().len();
    assert!(shown(&lint_alone) > shown(&whole));

    // A walk from `next` to `next` counts every pair once.
    let (mut counted, mut from) = (0, None::<String>);
    loop {
        let arguments = match &from {
            Some(from) => json!({ "section": "lint", "from": from }),
            None => json!({ "section": "lint" }),
        };
        let page = fitted(&mut client, "status", arguments);
        counted += page["counts"]["lint"]["duplicates"].as_u64().unwrap() as usize;
        let next = &page["counts"]["lint"]["pairs"]["next"];
        let Some(next) = next.as_str() else { break };
        from = Some(next.to_owned());
    }
    assert_eq!(counted, pairs);
}

/// Anchor findings past the budget: the section fits, keeps the
/// unresolved ones, which sort first, counts every finding by kind, and
/// says how many it leaves out and where the rest is.
#[test]
fn the_anchors_section_fits_the_budget_and_counts_what_it_leaves_out() {
    const UNRESOLVED: usize = 300;
    const AMBIGUOUS: usize = 300;
    let project = Project::init();
    project.source(
        "src/lib.rs",
        "pub struct A { pages: u8 }\npub struct B { pages: u8 }\n",
    );
    let gone: Vec<String> = (0..UNRESOLVED)
        .map(|i| format!("{{ file = \"src/gone/gone_{i:03}.rs\" }}"))
        .collect();
    project.write(
        "decisions/kept.toml",
        &decision(
            "project",
            "Kept",
            &format!("anchors = [{}]\n", gone.join(", ")),
        ),
    );
    let anchor = "anchors = [{ file = \"src/lib.rs\", symbol = \"pages\" }]\n";
    for i in 0..AMBIGUOUS {
        let name = format!("twice-{i:03}");
        project.write(
            &format!("decisions/{name}.toml"),
            &decision("project", &name, anchor),
        );
    }
    let (mut client, _) = McpClient::connect(&project);

    let status = fitted(&mut client, "status", json!({ "section": "anchors" }));

    assert!(status.get("truncated").is_none(), "{status}");
    let section = &status["anchors"];
    let shown = section["items"].as_array().unwrap();
    assert_eq!(section["total"], UNRESOLVED + AMBIGUOUS);
    assert!(
        !shown.is_empty() && shown.len() < UNRESOLVED,
        "{}",
        shown.len()
    );
    assert!(shown.iter().all(|item| item["kind"] == "unresolved"));
    assert_eq!(section["omitted"], UNRESOLVED + AMBIGUOUS - shown.len());
    assert_eq!(section["more"], "`trurlic check` lists every finding");
    assert_eq!(status["counts"]["anchors"]["unresolved"], UNRESOLVED);
    assert_eq!(status["counts"]["anchors"]["ambiguous"], AMBIGUOUS);
}
