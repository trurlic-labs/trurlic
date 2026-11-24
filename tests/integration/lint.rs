//! The `lint` section of `status` over a graph below its own bar, alike
//! over MCP and from the command line. The graph holds one decision per
//! quality rule and per field a pattern reads, and one pattern in a field
//! and the origin at once. It holds decisions with only file or directory
//! anchors, and clean ones that yield nothing, a file anchor beside a
//! symbol one among them. The default
//! call judges the limits alone and says so; a pattern that does not
//! compile leaves the limits judged and says so.

use serde_json::{Value, json};

use crate::harness::{McpClient, Project};

const TABLE: &str = "[quality]\nreason_words = { min = 5, max = 12 }\nalternatives_max = 2\n\
     alternative_words_max = 4\nanchors_max = 2\nrequire_origin = true\n\n\
     [[quality.patterns]]\npattern = 'zz'\n\
     fields = [\"choice\", \"reason\", \"alternatives\", \"tags\", \"anchors\", \"origin\"]\n\
     level = \"refuse\"\nmessage = \"No zz.\"\n\n\
     [[quality.patterns]]\npattern = 'yy'\n\
     fields = [\"choice\", \"reason\", \"alternatives\", \"tags\", \"anchors\", \"origin\"]\n\
     level = \"warn\"\nmessage = \"No yy.\"\n";

/// A decision at the fixture's bar, every field given: a reason of seven
/// words, one alternative of three, one anchor on a symbol, an origin.
struct Fields<'a> {
    choice: &'a str,
    reason: &'a str,
    alternatives: &'a [&'a str],
    tags: &'a [&'a str],
    anchors: &'a str,
    origin: Option<&'a str>,
}

const CLEAN: Fields<'static> = Fields {
    choice: "Keep the rule",
    reason: "A reason of exactly seven plain words.",
    alternatives: &["None - rejected: loose"],
    tags: &[],
    anchors: "{ file = \"src/lib.rs\", symbol = \"run\" }",
    origin: Some("task"),
};

fn text(fields: &Fields<'_>) -> String {
    let list = |items: &[&str]| {
        let quoted: Vec<String> = items.iter().map(|item| format!("{item:?}")).collect();
        format!("[{}]", quoted.join(", "))
    };
    let origin = fields
        .origin
        .map_or_else(String::new, |origin| format!("origin = {origin:?}\n"));
    format!(
        "component = \"project\"\nchoice = {:?}\nreason = {:?}\nalternatives = {}\n\
         tags = {}\nstatus = \"accepted\"\n{origin}created = \"2025-06-01T10:00:00Z\"\n\
         anchors = [{}]\n",
        fields.choice,
        fields.reason,
        list(fields.alternatives),
        list(fields.tags),
        fields.anchors
    )
}

/// The decisions of [`graph`].
const DECISIONS: usize = 19;

fn graph() -> Project {
    let project = Project::init();
    let header = project.read("project.toml");
    project.write("project.toml", &format!("{header}\n{TABLE}"));
    let symbol = "{ file = \"src/a.rs\", symbol = \"a\" }";
    let three = format!(
        "{symbol}, {{ file = \"src/b.rs\", symbol = \"b\" }}, {{ file = \"src/c.rs\", symbol = \"c\" }}"
    );
    let decisions: Vec<(&str, Fields<'_>)> = vec![
        ("clean", CLEAN),
        (
            "clean-without-origin",
            Fields {
                origin: None,
                ..CLEAN
            },
        ),
        (
            "few-words",
            Fields {
                reason: "Three words only.",
                ..CLEAN
            },
        ),
        (
            "many-words",
            Fields {
                reason: "One two three four five six seven eight nine ten eleven twelve thirteen.",
                ..CLEAN
            },
        ),
        (
            "no-alternative",
            Fields {
                alternatives: &[],
                ..CLEAN
            },
        ),
        (
            "many-alternatives",
            Fields {
                alternatives: &["A - no", "B - no", "C - no"],
                ..CLEAN
            },
        ),
        (
            "long-alternative",
            Fields {
                alternatives: &["Fine", "One two three four five"],
                ..CLEAN
            },
        ),
        (
            "no-anchor",
            Fields {
                anchors: "",
                ..CLEAN
            },
        ),
        (
            "many-anchors",
            Fields {
                anchors: &three,
                ..CLEAN
            },
        ),
        (
            "files-only",
            Fields {
                anchors: "{ file = \"src/a.rs\" }, { file = \"src/b.rs\" }",
                ..CLEAN
            },
        ),
        (
            "mixed-anchors",
            Fields {
                anchors: "{ file = \"src/a.rs\" }, { file = \"src/b.rs\", symbol = \"b\" }",
                ..CLEAN
            },
        ),
        (
            "directory-only",
            Fields {
                anchors: "{ file = \"src/store\" }",
                ..CLEAN
            },
        ),
        (
            "p-choice",
            Fields {
                choice: "Keep zz and yy",
                ..CLEAN
            },
        ),
        (
            "p-choice-origin",
            Fields {
                choice: "Keep zz",
                origin: Some("task zz"),
                ..CLEAN
            },
        ),
        (
            "p-reason",
            Fields {
                reason: "A reason that says zz and yy here.",
                ..CLEAN
            },
        ),
        (
            "p-alternatives",
            Fields {
                alternatives: &["Fine", "zz yy - no"],
                ..CLEAN
            },
        ),
        (
            "p-tags",
            Fields {
                tags: &["ok", "zz-yy"],
                ..CLEAN
            },
        ),
        (
            "p-anchors",
            Fields {
                anchors: "{ file = \"src/zz/yy.rs\", symbol = \"run\" }",
                ..CLEAN
            },
        ),
        (
            "p-origin",
            Fields {
                origin: Some("task zz yy"),
                ..CLEAN
            },
        ),
    ];
    assert_eq!(decisions.len(), DECISIONS);
    for (name, fields) in &decisions {
        project.write(&format!("decisions/{name}.toml"), &text(fields));
    }
    project
}

/// Each finding as `(decision, field, rule, level, measure or pattern)`.
fn findings(status: &Value) -> Vec<(String, String, String, String, Value)> {
    status["lint"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["kind"] != "duplicate")
        .map(|item| {
            let text = |key: &str| item[key].as_str().unwrap_or_default().to_owned();
            let detail = if item["kind"] == "file_anchors_only" {
                json!("files")
            } else if item.get("limit").is_some() {
                item["limit"].clone()
            } else {
                json!({ "pattern": item["pattern"], "at": item["at"] })
            };
            (
                text("decision"),
                text("field"),
                text("rule"),
                text("level"),
                detail,
            )
        })
        .collect()
}

fn limit(measured: usize, bound: usize, unit: &str) -> Value {
    json!({ "measured": measured, "bound": bound, "unit": unit })
}

#[test]
fn the_section_shows_every_rule_a_stored_decision_breaks() {
    let project = graph();
    project.run_ok(&["check"]);
    let (mut client, _) = McpClient::connect(&project);

    let text = client.call_tool_text("status", json!({ "section": "lint" }));
    let output = project.run_ok(&["status", "--json", "--section", "lint"]);
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("{text}\n")
    );
    let status: Value = serde_json::from_str(&text).unwrap();

    let at = |pattern: usize, at: usize| json!({ "pattern": pattern, "at": at });
    let row = |decision: &str, field: &str, rule: &str, level: &str, detail: Value| {
        (
            decision.into(),
            field.into(),
            rule.into(),
            level.into(),
            detail,
        )
    };
    let refuse = "refuse";
    let warn = "warn";
    let expected = vec![
        row(
            "few-words",
            "reason",
            "reason_words_min",
            refuse,
            limit(3, 5, "words"),
        ),
        row(
            "long-alternative",
            "alternatives[1]",
            "alternative_words_max",
            refuse,
            limit(5, 4, "words"),
        ),
        row(
            "many-alternatives",
            "alternatives",
            "alternatives_max",
            refuse,
            limit(3, 2, "alternatives"),
        ),
        row(
            "many-anchors",
            "anchors",
            "anchors_max",
            refuse,
            limit(3, 2, "anchors"),
        ),
        row(
            "many-words",
            "reason",
            "reason_words_max",
            refuse,
            limit(13, 12, "words"),
        ),
        row(
            "no-alternative",
            "alternatives",
            "alternative_required",
            refuse,
            limit(0, 1, "alternatives"),
        ),
        row(
            "no-anchor",
            "anchors",
            "anchor_required",
            refuse,
            limit(0, 1, "anchors"),
        ),
        row(
            "p-alternatives",
            "alternatives[1]",
            "pattern",
            refuse,
            at(0, 0),
        ),
        row(
            "p-alternatives",
            "alternatives[1]",
            "pattern",
            warn,
            at(1, 3),
        ),
        row("p-anchors", "anchors[0]", "pattern", refuse, at(0, 4)),
        row("p-anchors", "anchors[0]", "pattern", warn, at(1, 7)),
        row("p-choice", "choice", "pattern", refuse, at(0, 5)),
        row("p-choice", "choice", "pattern", warn, at(1, 12)),
        row("p-choice-origin", "origin", "pattern", refuse, at(0, 5)),
        row("p-choice-origin", "choice", "pattern", refuse, at(0, 5)),
        row("p-origin", "origin", "pattern", refuse, at(0, 5)),
        row("p-origin", "origin", "pattern", warn, at(1, 8)),
        row("p-reason", "reason", "pattern", refuse, at(0, 19)),
        row("p-reason", "reason", "pattern", warn, at(1, 26)),
        row("p-tags", "tags[1]", "pattern", refuse, at(0, 0)),
        row("p-tags", "tags[1]", "pattern", warn, at(1, 3)),
        row("directory-only", "", "", "", json!("files")),
        row("files-only", "", "", "", json!("files")),
    ];
    assert_eq!(findings(&status), expected);
    let counts = &status["counts"]["lint"];
    assert_eq!(
        (&counts["quality"], &counts["file_anchors_only"]),
        (&json!(21), &json!(2))
    );
    assert_eq!(
        counts["patterns"],
        json!({ "judged": DECISIONS, "unjudged": 0 })
    );

    let terminal = project.run_ok(&["status", "--section", "lint"]).stdout;
    let terminal = String::from_utf8(terminal).unwrap();
    for line in [
        "  `few-words` `reason`: reason_words_min, 3 words against 5 (refuse)\n",
        "  `p-tags` `tags[1]`: matches `[quality] patterns[1]` at byte 3 (warn)\n",
        "  `files-only`: only file anchors, none naming the part of its file it constrains\n",
    ] {
        assert!(terminal.contains(line), "{terminal}");
    }
}

/// `origin` narrows the findings to the decisions recorded under it.
#[test]
fn origin_narrows_the_findings() {
    let project = graph();
    let (mut client, _) = McpClient::connect(&project);

    let status = client.call_tool(
        "status",
        json!({ "section": "lint", "origin": "task zz yy" }),
    );

    let rules: Vec<String> = findings(&status)
        .into_iter()
        .map(|(name, ..)| name)
        .collect();
    assert_eq!(rules, ["p-origin", "p-origin"]);
}

/// The default call judges the limits alone: the same findings as the
/// section's but the patterns', and the counts say the patterns were not
/// judged and where they are. It compiles no pattern: one that does not
/// compile changes nothing in its answer.
#[test]
fn the_default_call_judges_the_limits_alone() {
    let project = graph();
    let (mut client, _) = McpClient::connect(&project);

    let text = client.call_tool_text("status", json!({}));
    let section = client.call_tool("status", json!({ "section": "lint" }));

    let status: Value = serde_json::from_str(&text).unwrap();
    let limits: Vec<_> = findings(&section)
        .into_iter()
        .filter(|row| row.2 != "pattern")
        .collect();
    assert_eq!(findings(&status), limits);
    let counts = &status["counts"]["lint"];
    assert_eq!(counts["quality"], 7);
    assert_eq!(
        counts["patterns"],
        json!({
            "judged": 0, "unjudged": DECISIONS, "why": "not_asked",
            "more": "the call judges the limits alone; `section: \"lint\"` judges the \
                     patterns too, within a work limit",
        })
    );
    let terminal = String::from_utf8(project.run_ok(&["status"]).stdout).unwrap();
    let line = "lint: the limits alone; `trurlic status --section lint` judges the 2 \
                [quality] pattern(s) too\n";
    assert!(terminal.contains(line), "{terminal}");

    let header = project.read("project.toml");
    project.write("project.toml", &header.replace("'yy'", "'(yy'"));
    assert_eq!(client.call_tool_text("status", json!({})), text);
}

/// A pattern that does not compile fails no call: the limits are judged,
/// no pattern is, and the counts name the pattern, which `check` reports.
#[test]
fn a_pattern_that_does_not_compile_leaves_the_limits_judged() {
    let project = graph();
    let header = project.read("project.toml");
    project.write("project.toml", &header.replace("'yy'", "'(yy'"));
    let (mut client, _) = McpClient::connect(&project);

    let status = client.call_tool("status", json!({ "section": "lint" }));

    let patterns = &status["counts"]["lint"]["patterns"];
    assert_eq!(
        (&patterns["judged"], &patterns["unjudged"], &patterns["why"]),
        (&json!(0), &json!(DECISIONS), &json!("uncompiled"))
    );
    let uncompiled = &patterns["uncompiled"];
    assert_eq!(uncompiled["index"], 1, "{status}");
    assert!(
        uncompiled["message"]
            .as_str()
            .unwrap()
            .contains("unclosed group")
    );
    assert_eq!(status["counts"]["lint"]["quality"], 7);
    assert!(findings(&status).iter().all(|row| row.2 != "pattern"));
    let stderr = project.run_refused(&["check"]);
    assert!(stderr.contains("does not compile"), "{stderr}");
}

/// A walk from `next` to `next` judges every decision of a graph whose
/// patterns' work passes the section's limit. The graph holds seventy
/// decisions of ten thousand bytes, every tenth ending in `zzq`, read by a
/// pattern of thousands of states that runs fast. The terminal names the
/// flag that reads on.
#[test]
fn from_reads_on_where_the_work_limit_stopped() {
    let project = Project::init();
    let header = project.read("project.toml");
    let table = "[quality]\n\n[[quality.patterns]]\npattern = '\\w{0,20}zzq'\n\
                 fields = [\"reason\"]\nlevel = \"warn\"\nmessage = \"No zzq.\"\n";
    project.write("project.toml", &format!("{header}\n{table}"));
    let words = "each decision keeps its reason in the graph ".repeat(225);
    for index in 0..70 {
        let tail = if index % 10 == 0 { " zzq" } else { "" };
        let fields = Fields {
            reason: &format!("{words}{tail}"),
            ..CLEAN
        };
        project.write(&format!("decisions/d{index:02}.toml"), &text(&fields));
    }
    let (mut client, _) = McpClient::connect(&project);

    let (mut from, mut pages, mut judged, mut matched) = (None::<String>, 0, 0, Vec::new());
    loop {
        let mut arguments = json!({ "section": "lint" });
        if let Some(from) = &from {
            arguments["from"] = json!(from);
        }
        let status = client.call_tool("status", arguments);
        let patterns = &status["counts"]["lint"]["patterns"];
        pages += 1;
        judged += patterns["judged"].as_u64().unwrap();
        matched.extend(findings(&status).into_iter().map(|row| row.0));
        match patterns["next"].as_str() {
            Some(next) => {
                assert_eq!(patterns["why"], "work_limit", "{patterns}");
                assert!(from.as_deref().is_none_or(|from| next > from), "{patterns}");
                from = Some(next.to_owned());
            }
            None => break,
        }
    }

    assert!(pages > 1, "{pages}");
    assert_eq!(judged, 70);
    let every_tenth: Vec<String> = (0..70).step_by(10).map(|i| format!("d{i:02}")).collect();
    assert_eq!(matched, every_tenth);
    let terminal = String::from_utf8(project.run_ok(&["status", "--section", "lint"]).stdout);
    let terminal = terminal.unwrap();
    assert!(
        terminal.contains("`trurlic status --section lint --from d"),
        "{terminal}"
    );
}
