//! The repair of stale anchors with real git: a symbol moved to another
//! file and a file renamed, both shown by `check`, and both repaired by
//! one `repair_anchors` op with no `repairs`, over MCP and from a batch
//! file alike, the index's cache filled on the way; such an op holds
//! against the ops before it in its batch, and refuses nothing when it
//! finds nothing to repair; a file anchor does not follow a rename git
//! pairs by similarity alone; explicit repairs the graph no longer matches
//! are refused, naming the repair; and a result of many repairs fits the
//! budget, counting what it leaves out.

use std::fs;

use serde_json::{Value, json};

use crate::harness::{McpClient, Project, decision, git};

const MAX_TOOL_RESULT_BYTES: usize = 24 * 1024;

/// Lines enough for git to find the renamed file similar to its source.
const OLD: &str = "pub fn kept() {}\npub fn a() {}\npub fn b() {}\npub fn c() {}\npub fn d() {}\n";

/// A project of one Cargo package, whose manifest the search for a moved
/// symbol stays within.
fn package() -> Project {
    let project = Project::init();
    project.source("Cargo.toml", "[package]\nname = \"p\"\n");
    project
}

/// A tracked graph whose accepted decisions anchor `src/lib.rs::moved` and
/// `src/old.rs::kept` and `src/old.rs`, committed with the code; then
/// `moved` moved to `src/other.rs` and `src/old.rs` renamed to
/// `src/new.rs` unchanged, committed.
fn moved_and_renamed() -> Project {
    let project = package();
    project.source("src/lib.rs", "pub fn moved() {}\npub fn stay() {}\n");
    project.source("src/old.rs", OLD);
    let anchors = |list: &str| format!("anchors = [{list}]\n");
    project.write(
        "decisions/moved-rule.toml",
        &decision(
            "project",
            "Moved",
            &anchors("{ file = \"src/lib.rs\", symbol = \"moved\" }"),
        ),
    );
    project.write(
        "decisions/renamed-rule.toml",
        &decision(
            "project",
            "Renamed",
            &anchors("{ file = \"src/old.rs\", symbol = \"kept\" }, { file = \"src/old.rs\" }"),
        ),
    );
    project.run_ok(&["fmt"]);
    git(project.path(), &["add", "--all"]);
    git(project.path(), &["commit", "--quiet", "-m", "code"]);
    project.source("src/lib.rs", "pub fn stay() {}\n");
    project.source("src/other.rs", "pub fn moved() {}\n");
    git(project.path(), &["mv", "src/old.rs", "src/new.rs"]);
    git(project.path(), &["add", "--all"]);
    git(project.path(), &["commit", "--quiet", "-m", "move"]);
    project
}

fn every_repair() -> Value {
    json!({ "origin": "repair", "ops": [{ "op": "repair_anchors" }] })
}

/// `check` shows each repair on its stale line and fails, and `status`,
/// which runs no git, none; one op with no `repairs` writes exactly
/// those, the same bytes over MCP and from a file, reading the tree before
/// it takes the lock, so the index's cache fills; `check` passes after it,
/// and the op run again finds nothing to repair and refuses nothing.
#[test]
fn a_moved_symbol_and_a_renamed_file_are_repaired_by_one_op() {
    let (over_mcp, from_file) = (moved_and_renamed(), moved_and_renamed());
    let stderr = over_mcp.run_refused(&["check"]);
    let lines = [
        "  error: anchor `src/lib.rs::moved` of `moved-rule` is stale: its file defines nothing \
         its symbol names; repair: `src/other.rs::moved`",
        "  error: anchor `src/old.rs` of `renamed-rule` is stale: nothing is at its path; \
         repair: `src/new.rs`",
        "  error: anchor `src/old.rs::kept` of `renamed-rule` is stale: nothing is at its path; \
         repair: `src/new.rs::kept`",
    ];
    assert_eq!(
        stderr,
        format!(
            "{}\nerror: 3 consistency error(s) found\n",
            lines.join("\n")
        )
    );
    let (mut client, _) = McpClient::connect(&over_mcp);
    let status = client.call_tool("status", json!({ "section": "anchors" }));
    let item = &status["anchors"]["items"][0];
    assert_eq!(item["kind"], "unresolved", "{status}");
    assert!(item.get("repair").is_none(), "{status}");
    let cache = from_file.node(".state/symbols");
    assert!(!cache.exists());

    let text = client.call_tool_text("apply", every_repair());
    let output = from_file.apply(&every_repair(), &["--json"]);

    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("{text}\n")
    );
    // The project's name is its directory's, which the two do not share.
    let decisions = |project: &Project| {
        let mut files = project.graph_files();
        files.remove("project.toml");
        files
    };
    assert_eq!(decisions(&over_mcp), decisions(&from_file));
    let result: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(result["ops"][0]["op"], "repair_anchors");
    assert_eq!(
        result["repaired"],
        json!([
            { "decision": "moved-rule", "anchor": "src/lib.rs::moved", "repair": "src/other.rs::moved" },
            { "decision": "renamed-rule", "anchor": "src/old.rs", "repair": "src/new.rs" },
            { "decision": "renamed-rule", "anchor": "src/old.rs::kept", "repair": "src/new.rs::kept" },
        ])
    );
    assert!(result.get("left").is_none(), "{result}");
    assert!(fs::read_dir(&cache).unwrap().count() > 0);
    let decision = over_mcp.read("decisions/renamed-rule.toml");
    assert!(decision.contains("status = \"accepted\""), "{decision}");
    over_mcp.run_ok(&["check"]);
    let again = client.call_tool("apply", every_repair());
    assert_eq!(
        again,
        json!({
            "ops": [{ "op": "repair_anchors", "name": "" }],
            "written": 0, "removed": 0,
        })
    );
}

/// The repairs an op asking for every one writes depend on the anchors
/// alone, applied where the op runs: after an earlier op of its batch
/// renamed a decision, removed a stale anchor, or repaired it already,
/// each anchor still stale is repaired once and nothing is refused.
#[test]
fn every_repair_holds_against_the_ops_before_it() {
    let rename = json!({ "op": "rename", "name": "renamed-rule", "to": "renamed-again" });
    let remove = json!({
        "op": "revise", "name": "renamed-rule",
        "remove_anchors": [{ "file": "src/old.rs", "symbol": "kept" }],
    });
    let every = json!({ "op": "repair_anchors" });
    let cases = [
        (rename, "renamed-again", 3),
        (remove, "renamed-rule", 2),
        (every.clone(), "renamed-rule", 3),
    ];
    for (first, decision, repaired) in cases {
        let project = moved_and_renamed();
        let (mut client, _) = McpClient::connect(&project);

        let batch = json!({ "origin": "repair", "ops": [first, every] });
        let result = client.call_tool("apply", batch);

        assert_eq!(
            result["repaired"].as_array().map(Vec::len),
            Some(repaired),
            "{result}"
        );
        let text = project.read(&format!("decisions/{decision}.toml"));
        assert!(text.contains("src/new.rs"), "{text}");
        assert!(!text.contains("src/old.rs"), "{text}");
        project.run_ok(&["check"]);
    }
}

/// An op asking for every repair that finds none refuses nothing: the
/// rest of its batch is written, and a stale anchor with no repair is
/// still listed as left, nothing written for it.
#[test]
fn every_repair_with_none_to_make_refuses_nothing() {
    let project = package();
    project.write(
        "decisions/kept.toml",
        &decision("project", "Kept", "anchors = [{ file = \"src/lib.rs\" }]\n"),
    );
    let (mut client, _) = McpClient::connect(&project);
    let tag = json!({ "op": "revise", "name": "kept", "add_tags": ["t"] });

    let tagged = client.call_tool(
        "apply",
        json!({ "origin": "repair", "ops": [tag, { "op": "repair_anchors" }] }),
    );
    project.write(
        "decisions/gone.toml",
        &decision(
            "project",
            "Gone",
            "anchors = [{ file = \"src/gone.rs\" }]\n",
        ),
    );
    let left = client.call_tool("apply", every_repair());

    assert_eq!(tagged["written"], 1, "{tagged}");
    assert!(project.read("decisions/kept.toml").contains("\"t\""));
    assert_eq!(left["written"], 0, "{left}");
    assert_eq!(
        left["left"],
        json!([{ "decision": "gone", "anchor": "src/gone.rs", "cause": "nothing is at its path" }])
    );
}

/// git pairs a deleted file with a new one by similarity alone: two files
/// that share a header and nothing else are paired, yet the file anchor
/// does not follow, since nothing verifies the pairing, and the symbol
/// anchor, which names nothing there, gets no repair either.
#[test]
fn a_file_anchor_does_not_follow_a_rename_git_pairs_by_a_shared_header() {
    let project = package();
    let header: String = (0..12).map(|n| format!("// License line {n}.\n")).collect();
    project.source(
        "src/config.rs",
        &format!("{header}pub fn parse_config() {{}}\n"),
    );
    project.write(
        "decisions/cfg.toml",
        &decision(
            "project",
            "Config",
            "anchors = [{ file = \"src/config.rs\" }, \
             { file = \"src/config.rs\", symbol = \"parse_config\" }]\n",
        ),
    );
    project.run_ok(&["fmt"]);
    git(project.path(), &["add", "--all"]);
    git(project.path(), &["commit", "--quiet", "-m", "config"]);
    git(project.path(), &["rm", "--quiet", "src/config.rs"]);
    project.source(
        "src/html.rs",
        &format!("{header}pub fn render_html() {{}}\n"),
    );
    git(project.path(), &["add", "--all"]);
    git(project.path(), &["commit", "--quiet", "-m", "html"]);

    let stderr = project.run_refused(&["check"]);

    assert!(
        stderr.contains("`src/config.rs` of `cfg` is stale"),
        "{stderr}"
    );
    assert!(!stderr.contains("repair"), "{stderr}");
}

/// A stale anchor with no repair is listed as left beside the repairs.
#[test]
fn a_stale_anchor_with_no_repair_is_listed_as_left() {
    let project = moved_and_renamed();
    let text = project.read("decisions/moved-rule.toml");
    project.write(
        "decisions/moved-rule.toml",
        &text.replace("\"moved\"", "\"vanished\""),
    );
    let (mut client, _) = McpClient::connect(&project);

    let result = client.call_tool("apply", every_repair());

    assert_eq!(result["repaired"].as_array().unwrap().len(), 2, "{result}");
    assert_eq!(
        result["left"],
        json!([{
            "decision": "moved-rule", "anchor": "src/lib.rs::vanished",
            "cause": "its file defines nothing its symbol names",
        }])
    );
}

/// Repairs given explicitly are held to the graph as it is when the batch
/// runs: one whose stale anchor a hand edit removed meanwhile is refused,
/// naming it, and nothing is written.
#[test]
fn a_repair_the_graph_no_longer_matches_is_refused() {
    let project = moved_and_renamed();
    let before = project.graph_files();
    let text = project.read("decisions/moved-rule.toml");
    project.write(
        "decisions/moved-rule.toml",
        &text.replace("src/lib.rs", "src/stay.rs"),
    );
    let edited = project.graph_files();
    assert_ne!(edited, before);
    let (mut client, _) = McpClient::connect(&project);

    let refusal = client.call_tool_refusal(
        "apply",
        json!({ "origin": "repair", "ops": [{ "op": "repair_anchors", "repairs": [{
            "decision": "moved-rule",
            "from": { "file": "src/lib.rs", "symbol": "moved" },
            "to": { "file": "src/other.rs", "symbol": "moved" },
        }]}]}),
    );

    assert_eq!(refusal["kind"], "batch_refused", "{refusal}");
    assert_eq!(refusal["rule"], "not_held");
    assert_eq!(refusal["field"], "ops[0].repairs[0].from");
    assert_eq!(project.graph_files(), edited);
}

/// A result of more repairs than the budget holds lists those that fit,
/// in order, and counts the rest: 600 functions moved, committed, from a
/// file the move deleted.
#[test]
fn many_repairs_fit_the_budget_and_count_the_rest() {
    const ANCHORS: usize = 600;
    let project = package();
    let functions: String = (0..ANCHORS)
        .map(|n| format!("pub fn function_{n:03}() {{}}\n"))
        .collect();
    project.source("src/gone.rs", &functions);
    let anchors: Vec<String> = (0..ANCHORS)
        .map(|n| format!("{{ file = \"src/gone.rs\", symbol = \"function_{n:03}\" }}"))
        .collect();
    project.write(
        "decisions/many.toml",
        &decision(
            "project",
            "Many",
            &format!("anchors = [{}]\n", anchors.join(", ")),
        ),
    );
    project.run_ok(&["fmt"]);
    git(project.path(), &["add", "--all"]);
    git(project.path(), &["commit", "--quiet", "-m", "gone"]);
    git(project.path(), &["rm", "--quiet", "src/gone.rs"]);
    project.source("src/lib.rs", &functions);
    git(project.path(), &["add", "--all"]);
    git(project.path(), &["commit", "--quiet", "-m", "moved"]);
    let (mut client, _) = McpClient::connect(&project);

    let text = client.call_tool_text("apply", every_repair());

    assert!(text.len() <= MAX_TOOL_RESULT_BYTES, "{} bytes", text.len());
    let result: Value = serde_json::from_str(&text).unwrap();
    let listed = result["repaired"].as_array().unwrap().len();
    assert!(listed > 0 && listed < ANCHORS, "{listed}");
    assert_eq!(result["more_repaired"], ANCHORS - listed);
    assert_eq!(result["repaired"][0]["repair"], "src/lib.rs::function_000");
    project.run_ok(&["check"]);
}

/// A project of one package whose accepted decision `d` anchors each of
/// `anchors`, with `before` written and committed, then `after` (a path
/// and its text, or `None` to remove it) committed.
fn changed(anchors: &[&str], before: &[(&str, &str)], after: &[(&str, Option<&str>)]) -> Project {
    let project = package();
    for (file, text) in before {
        project.source(file, text);
    }
    let spelled: Vec<String> = anchors
        .iter()
        .map(|anchor| match anchor.split_once("::") {
            Some((file, symbol)) => format!("{{ file = \"{file}\", symbol = \"{symbol}\" }}"),
            None => format!("{{ file = \"{anchor}\" }}"),
        })
        .collect();
    let extra = format!("anchors = [{}]\n", spelled.join(", "));
    project.write("decisions/d.toml", &decision("project", "Rule d", &extra));
    project.run_ok(&["fmt"]);
    git(project.path(), &["add", "--all"]);
    git(project.path(), &["commit", "--quiet", "-m", "before"]);
    for (file, text) in after {
        match text {
            Some(text) => project.source(file, text),
            None => fs::remove_file(project.path().join(file)).unwrap(),
        }
    }
    git(project.path(), &["add", "--all"]);
    git(project.path(), &["commit", "--quiet", "-m", "after"]);
    project
}

/// A wrong repair is worse than none: a namesake that stood beside the
/// deleted code (another file's function, a test module's helper, another
/// file's type), a header-only or an empty file git pairs with the removed
/// one, and a binary git pairs with another by shared boilerplate, whose
/// `main` it edited, are each no repair; `check` shows none and the op
/// writes none.
#[test]
fn a_namesake_or_a_rename_that_carried_no_code_is_never_written() {
    let header = "// Licensed under the terms the project states.\n// Copyright the authors.\n";
    let boilerplate: String = (0..12)
        .map(|n| format!("use std::io::Line{n};\n"))
        .collect();
    let import = format!("{boilerplate}fn main() {{ import() }}\n");
    let report = format!("{boilerplate}fn main() {{ report() }}\n");
    let cases = [
        changed(
            &["src/http.rs::parse_header"],
            &[
                ("src/http.rs", "pub fn parse_header() {}\n"),
                ("src/mail.rs", "pub fn parse_header() {}\n"),
            ],
            &[("src/http.rs", None)],
        ),
        changed(
            &["src/cli.rs::run"],
            &[
                ("src/cli.rs", "pub fn run() {}\npub fn main() {}\n"),
                (
                    "src/parse.rs",
                    "pub fn parse() {}\n#[cfg(test)]\nmod tests { fn run() {} }\n",
                ),
            ],
            &[("src/cli.rs", Some("pub fn main() {}\n"))],
        ),
        changed(
            &["src/model.rs::Message"],
            &[
                ("src/model.rs", "pub struct Message;\n"),
                ("src/proto.rs", "pub struct Message;\n"),
            ],
            &[("src/model.rs", Some("pub struct Envelope;\n"))],
        ),
        changed(
            &["src/alpha.rs"],
            &[("src/alpha.rs", header)],
            &[("src/alpha.rs", None), ("src/beta.rs", Some(header))],
        ),
        changed(
            &["src/void.rs"],
            &[("src/void.rs", "")],
            &[("src/void.rs", None), ("src/empty.rs", Some(""))],
        ),
        changed(
            &["src/bin/import.rs", "src/bin/import.rs::main"],
            &[("src/bin/import.rs", &import)],
            &[
                ("src/bin/import.rs", None),
                ("src/bin/report.rs", Some(&report)),
            ],
        ),
    ];
    for project in cases {
        let stderr = project.run_refused(&["check"]);
        let output = project.apply(&every_repair(), &["--json"]);

        assert!(stderr.contains("is stale"), "{stderr}");
        assert!(!stderr.contains("repair:"), "{stderr}");
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(result.get("repaired").is_none(), "{result}");
        assert_eq!(result["written"], 0, "{result}");
    }
}

/// What an op asking for every repair leaves is stated against the graph
/// the write left: a decision an earlier op renamed is listed under its new
/// name, with what it was repaired as and what it was left with.
#[test]
fn what_every_repair_leaves_is_stated_against_the_written_graph() {
    let project = changed(
        &["src/gone.rs", "src/a.rs::f"],
        &[
            ("src/gone.rs", "pub fn g() {}\n"),
            ("src/a.rs", "pub fn f() {}\n"),
        ],
        &[
            ("src/gone.rs", None),
            ("src/a.rs", Some("")),
            ("src/b.rs", Some("pub fn f() {}\n")),
        ],
    );
    let (mut client, _) = McpClient::connect(&project);
    let rename = json!({ "op": "rename", "name": "d", "to": "d2" });

    let result = client.call_tool(
        "apply",
        json!({ "origin": "repair", "ops": [rename, { "op": "repair_anchors" }] }),
    );

    assert_eq!(
        result["repaired"],
        json!([{ "decision": "d2", "anchor": "src/a.rs::f", "repair": "src/b.rs::f" }])
    );
    assert_eq!(
        result["left"],
        json!([{ "decision": "d2", "anchor": "src/gone.rs", "cause": "nothing is at its path" }])
    );
}
