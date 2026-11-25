//! `apply` over MCP and as `trurlic apply`, over the built binary: one
//! batch writes the same bytes through either surface, a batch with one
//! invalid op writes nothing and names that op, and the command reads a
//! file or stdin and prints a line per op or the tool's JSON.

use std::io::Write;
use std::process::Stdio;

use serde_json::{Value, json};

use crate::harness::{GraphFiles, McpClient, Project, record};

/// Every op kind, each building on the ones before it.
fn every_op() -> Value {
    json!({ "origin": "task 18", "ops": [
        { "op": "add_component", "name": "auth", "description": "Sign-in" },
        { "op": "add_component", "name": "ledger", "description": "Books" },
        { "op": "add_component", "name": "scratch", "description": "" },
        {
            "op": "record", "name": "sign-tokens", "component": "auth",
            "choice": "Sign tokens with Ed25519",
            "reason": "Verification needs no session store",
            "alternatives": ["HMAC: rejected, a shared secret"],
            "tags": ["security"],
            "anchors": [{ "file": "src/auth.rs", "symbol": "sign" }],
        },
        {
            "op": "record", "name": "rotate-keys", "component": "auth",
            "choice": "Rotate keys weekly", "reason": "A leaked key expires within a week",
            "alternatives": ["Never rotate: rejected, a leak lasts"],
            "depends_on": ["sign-tokens"], "anchors": [{ "file": "src/rotate.rs" }],
        },
        {
            "op": "record", "name": "old-rotation", "component": "auth",
            "choice": "Rotate keys monthly", "reason": "The first plan of the rotation",
            "alternatives": ["Weekly: rejected, too often"],
            "depends_on": ["sign-tokens"], "anchors": [{ "file": "src/keys.rs" }],
        },
        { "op": "revise", "name": "rotate-keys", "add_tags": ["keys"] },
        { "op": "merge", "survivor": "rotate-keys", "absorbed": "old-rotation" },
        { "op": "rename", "name": "rotate-keys", "to": "rotate-signing-keys" },
        { "op": "revise_component", "name": "auth", "add_connects_to": ["ledger"] },
        { "op": "rename_component", "name": "ledger", "to": "billing" },
        {
            "op": "record", "name": "temp", "component": "billing",
            "choice": "A placeholder", "reason": "The next op removes it again",
            "alternatives": ["None"], "anchors": [{ "file": "src" }],
        },
        { "op": "remove", "name": "temp" },
        { "op": "remove_component", "name": "scratch" },
    ]})
}

/// The node files of `files` with each `created` instant blanked: the
/// clock's, which two runs cannot share. `project.toml` names the
/// project's directory and no batch writes it.
fn without_instants(files: GraphFiles) -> GraphFiles {
    files
        .into_iter()
        .filter(|(path, _)| path != "project.toml")
        .map(|(path, text)| {
            let lines: Vec<&str> = text
                .lines()
                .map(|line| {
                    if line.starts_with("created = ") {
                        "created = \"\""
                    } else {
                        line
                    }
                })
                .collect();
            (path, lines.join("\n"))
        })
        .collect()
}

#[test]
fn one_batch_writes_the_same_bytes_over_mcp_and_from_a_file() {
    let over_mcp = Project::init();
    let (mut client, _) = McpClient::connect(&over_mcp);
    let mcp_text = client.call_tool_text("apply", every_op());
    let from_file = Project::init();

    let output = from_file.apply(&every_op(), &["--json"]);

    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("{mcp_text}\n")
    );
    let written = over_mcp.graph_files();
    assert_eq!(
        without_instants(written.clone()),
        without_instants(from_file.graph_files())
    );
    assert_eq!(
        written.keys().collect::<Vec<_>>(),
        [
            "components/auth.toml",
            "components/billing.toml",
            "decisions/rotate-signing-keys.toml",
            "decisions/sign-tokens.toml",
            "project.toml",
        ]
    );
    let merged = &written["decisions/rotate-signing-keys.toml"];
    assert!(merged.contains("status = \"proposed\""), "{merged}");
    assert!(merged.contains("origin = \"task 18\""), "{merged}");
    assert!(merged.contains("{ file = \"src/keys.rs\" }"), "{merged}");
    from_file.run_ok(&["check"]);
}

/// Fifty records, the thirty-eighth naming a component that does not
/// exist: neither surface writes a file, and both name that op.
#[test]
fn a_fifty_op_batch_with_one_invalid_op_writes_nothing_and_names_it() {
    let ops: Vec<Value> = (0..50)
        .map(|index| {
            let component = if index == 37 { "nowhere" } else { "project" };
            record(
                &format!("rule-{index}"),
                component,
                &format!("Rule {index}"),
            )
        })
        .collect();
    let batch = json!({ "ops": ops });
    let project = Project::init();
    let before = project.graph_files();

    let (mut client, _) = McpClient::connect(&project);
    let refusal = client.call_tool_refusal("apply", batch.clone());
    let output = project.apply(&batch, &[]);

    assert_eq!(project.graph_files(), before);
    assert_eq!(refusal["kind"], "batch_refused", "{refusal}");
    assert_eq!(refusal["rule"], "component_missing");
    assert_eq!(refusal["field"], "ops[37].component");
    assert_eq!(
        refusal["op"],
        json!({ "index": 37, "op": "record", "name": "rule-37" })
    );
    let message = "ops[37] (record `rule-37`), `component`: names `nowhere`, which is no component";
    assert_eq!(refusal["message"], message);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        format!("error: {message}\n")
    );

    let valid = batch.to_string().replace("nowhere", "project");
    let applied = client.call_tool_text("apply", serde_json::from_str(&valid).unwrap());
    let applied: Value = serde_json::from_str(&applied).unwrap();
    assert_eq!(applied["ops"].as_array().unwrap().len(), 50);
    assert_eq!(applied["written"], 50);
    assert!(applied.get("truncated").is_none());
}

/// The result is `structuredContent` and the same JSON as text.
#[test]
fn a_result_is_structured_content_and_the_same_json_as_text() {
    let project = Project::init();
    let (mut client, _) = McpClient::connect(&project);
    let batch = json!({ "ops": [
        { "op": "add_component", "name": "auth", "description": "Sign-in" },
    ]});

    let result = client.request("tools/call", json!({ "name": "apply", "arguments": batch }));

    let text = result["content"][0]["text"].as_str().unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(text).unwrap(),
        result["structuredContent"]
    );
    assert_eq!(
        result["structuredContent"],
        json!({ "ops": [{ "op": "add_component", "name": "auth" }], "written": 1, "removed": 0 })
    );
}

#[test]
fn a_batch_from_stdin_prints_a_line_per_op() {
    let project = Project::init();
    let batch = json!({ "ops": [
        { "op": "add_component", "name": "auth", "description": "Sign-in" },
        {
            "op": "record", "name": "sign", "component": "auth",
            "choice": "Sign tokens", "reason": "Verification needs no store",
            "alternatives": ["HMAC"], "anchors": [{ "file": "src/auth.rs" }],
        },
        { "op": "revise", "name": "sign", "choice": "Sign every token", "add_tags": ["keys"] },
        { "op": "rename", "name": "sign", "to": "sign-tokens" },
    ]});
    let mut child = project
        .command()
        .args(["apply", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(batch.to_string().as_bytes())
        .unwrap();

    let output = child.wait_with_output().unwrap();

    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "added component `auth`\nrecorded `sign`\nrevised `sign`: choice, tags\n\
         renamed `sign` to `sign-tokens`\n2 file(s) written, 0 removed\n"
    );
}

/// A file that holds no batch is refused: a file that is not JSON, JSON
/// that is no object, and an object the spec refuses, with `--json`
/// printing the refusal the tool would return.
#[test]
fn a_file_that_holds_no_batch_is_refused() {
    let project = Project::init();
    let file = project.path().join("batch.json");
    for (text, message) in [
        ("{ops", "a batch is JSON: "),
        ("[]", "a batch is a JSON object of `ops` and `origin`"),
        (
            r#"{"ops": [], "extra": 1}"#,
            "`extra` is not declared by this tool",
        ),
    ] {
        std::fs::write(&file, text).unwrap();
        let stderr = project.run_refused(&["apply", file.to_str().unwrap()]);
        assert!(stderr.starts_with(&format!("error: {message}")), "{stderr}");
    }

    let output = project.apply(&json!({ "ops": "none" }), &["--json"]);
    assert_eq!(output.status.code(), Some(1));
    let refusal: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        refusal,
        json!({ "error": {
            "kind": "invalid_argument", "field": "ops", "message": "`ops` must be an array",
        }})
    );
}

/// `status --json` prints what the tool returns; `origin` narrows the
/// counts to the decisions recorded under it.
#[test]
fn status_on_the_command_line_is_the_tool() {
    let project = Project::init();
    let record = |name: &str| record(name, "project", &format!("Rule {name}"));
    project.apply(
        &json!({ "origin": "row 1", "ops": [record("a"), record("b")] }),
        &[],
    );
    project.apply(&json!({ "origin": "row 2", "ops": [record("c")] }), &[]);
    let (mut client, _) = McpClient::connect(&project);

    for (flags, arguments) in [
        (vec![], json!({})),
        (
            vec!["--origin", "row 1", "--section", "components"],
            json!({ "origin": "row 1", "section": "components" }),
        ),
    ] {
        let tool = client.call_tool_text("status", arguments);
        let mut args = vec!["status", "--json"];
        args.extend(flags);
        let command = String::from_utf8(project.run_ok(&args).stdout).unwrap();
        assert_eq!(command, format!("{tool}\n"));
    }
    let narrowed = client.call_tool("status", json!({ "origin": "row 1" }));
    assert_eq!(narrowed["counts"]["decisions"], 2, "{narrowed}");
    assert_eq!(narrowed["counts"]["proposed"], 2, "{narrowed}");
    assert_eq!(
        narrowed["components"]["items"],
        json!([{ "name": "project", "decisions": 2, "proposed": 2 }])
    );
    let text = String::from_utf8(project.run_ok(&["status"]).stdout).unwrap();
    assert!(
        text.contains("decisions: 3 (3 project-wide, 3 proposed)\n"),
        "{text}"
    );
}

/// A record of `name` with `choice` and `reason`, at the built-in bar.
fn worded(name: &str, choice: &str, reason: &str) -> Value {
    json!({
        "op": "record", "name": name, "component": "project", "choice": choice,
        "reason": reason, "alternatives": ["No rule: rejected"],
        "anchors": [{ "file": "src/lib.rs" }],
    })
}

/// The batches of the duplicates test, in order: two unrelated decisions;
/// a record that restates the first; a revise of the second's reason that
/// restates it too; a revise of anchors alone.
fn duplicate_batches() -> [Value; 4] {
    let lock = "Every write takes the exclusive lock";
    let why = "Two writers that race on one file lose an update without the lock.";
    [
        json!({ "ops": [
            worded("lock-writes", lock, why),
            worded("retry", "Retry a failed request with backoff", "A blip passes."),
        ]}),
        json!({ "ops": [worded("writes-lock", lock, "Writers race on one file without it.")] }),
        json!({ "ops": [{ "op": "revise", "name": "retry", "choice": lock, "reason": why }] }),
        json!({ "ops": [{ "op": "revise", "name": "retry", "add_anchors": [{ "file": "src/b.rs" }] }] }),
    ]
}

/// A likely duplicate warns and never refuses, alike over both surfaces:
/// on a recorded decision and on one whose text a revise changed, with its
/// nearest best first, and not on a revise of anchors alone. A terminal
/// gets the warning on stderr, and `status` ranks the pairs under `lint`.
#[test]
fn a_likely_duplicate_warns_over_both_surfaces() {
    let over_mcp = Project::init();
    let (mut client, _) = McpClient::connect(&over_mcp);
    let from_file = Project::init();

    let mut answers = Vec::new();
    for batch in duplicate_batches() {
        let text = client.call_tool_text("apply", batch.clone());
        let output = from_file.apply(&batch, &["--json"]);
        assert!(output.status.success(), "{output:?}");
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!("{text}\n")
        );
        answers.push(serde_json::from_str::<Value>(&text).unwrap());
    }

    assert!(answers[0].get("duplicates").is_none(), "{}", answers[0]);
    assert_eq!(
        answers[1]["duplicates"],
        json!([{ "decision": "writes-lock", "nearest": [
            { "decision": "lock-writes", "score": "0.815672" },
        ]}])
    );
    assert_eq!(
        answers[2]["duplicates"],
        json!([{ "decision": "retry", "nearest": [
            { "decision": "lock-writes", "score": "1.000000" },
            { "decision": "writes-lock", "score": "0.820476" },
        ]}])
    );
    assert!(answers[3].get("duplicates").is_none(), "{}", answers[3]);

    let terminal = Project::init();
    let [first, second, ..] = duplicate_batches();
    terminal.apply(&first, &[]);
    let output = terminal.apply(&second, &[]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "warning: `writes-lock` is a likely duplicate of `lock-writes` (0.815672)\n"
    );
    let status = from_file.run_ok(&["status", "--section", "lint"]).stdout;
    let status = String::from_utf8(status).unwrap();
    // Their anchors name files alone: three findings of that kind follow.
    assert!(
        status.contains(
            "lint: 6 (3 likely duplicate pair(s), 0 quality finding(s), 3 decision(s) with \
             only file anchors)\n  likely duplicates (1.000000): `lock-writes` and `retry`\n  \
             likely duplicates (0.820476): `lock-writes` and `writes-lock`\n  \
             likely duplicates (0.820476): `retry` and `writes-lock`\n"
        ),
        "{status}"
    );
}

/// `--section` takes exactly the sections the tool's schema lists: each
/// one prints what the call returns, and any other is refused naming them.
#[test]
fn the_section_flag_takes_the_tools_sections() {
    let project = Project::init();
    let (mut client, _) = McpClient::connect(&project);
    let listed = client.request("tools/list", json!({}));
    let status = listed["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "status")
        .unwrap();
    let sections: Vec<&str> = status["inputSchema"]["properties"]["section"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|section| section.as_str().unwrap())
        .collect();

    for &section in &sections {
        let tool = client.call_tool_text("status", json!({ "section": section }));
        let output = project.run_ok(&["status", "--json", "--section", section]);
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!("{tool}\n")
        );
    }
    let refused = project
        .command()
        .args(["status", "--section", "everything"])
        .output()
        .unwrap();
    assert_eq!(refused.status.code(), Some(2), "{refused:?}");
    let stderr = String::from_utf8(refused.stderr).unwrap();
    let possible = format!("[possible values: {}]", sections.join(", "));
    assert!(stderr.contains(&possible), "{stderr}");
}

/// `[quality]` of project.toml holds both surfaces alike: a refusal names
/// the rule, the field and the measure; a `warn` pattern writes and warns,
/// in `warnings` over MCP and on stderr for a terminal; `status` counts the
/// rules; a pattern that does not compile fails `check` and every write,
/// while `status`, a read, still answers.
#[test]
fn the_quality_table_holds_both_surfaces_alike() {
    let project = Project::init();
    let header = project.read("project.toml");
    let table = "[quality]\nreason_words = { max = 5 }\nrequire_origin = true\n\n\
                 [[quality.patterns]]\npattern = 'zz'\nfields = [\"choice\"]\nlevel = \"warn\"\n\
                 message = \"No zz.\"\n";
    project.write("project.toml", &format!("{header}\n{table}"));
    let mut long = record("long", "project", "A rule");
    long["reason"] = json!("one two three four five six");
    let long = json!({ "origin": "row 1", "ops": [long] });
    let (mut client, _) = McpClient::connect(&project);

    let refusal = client.call_tool_refusal("apply", long.clone());
    let output = project.apply(&long, &[]);

    let message = "ops[0] (record `long`), `reason`: 6 words, over the maximum of 5 \
                   (`[quality] reason_words.max`)";
    assert_eq!(
        (&refusal["rule"], &refusal["field"], &refusal["message"]),
        (
            &json!("reason_words_max"),
            &json!("ops[0].reason"),
            &json!(message)
        )
    );
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        format!("error: {message}\n")
    );
    let unnamed = client.call_tool_refusal(
        "apply",
        json!({ "ops": [record("plain", "project", "A rule")] }),
    );
    assert_eq!(
        (&unnamed["rule"], &unnamed["field"]),
        (&json!("origin_required"), &json!("origin"))
    );

    let warned =
        |name: &str| json!({ "origin": "row 1", "ops": [record(name, "project", "Rule zz")] });
    let result = client.call_tool("apply", warned("zz-one"));
    assert_eq!(
        (&result["warnings"], &result["patterns"]),
        (
            &json!([{ "field": "ops[0].choice", "rule": "pattern", "pattern": 0, "at": 5 }]),
            &json!([{ "index": 0, "message": "No zz." }])
        )
    );
    let output = project.apply(&warned("zz-two"), &[]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "warning: record `zz-two`, `ops[0].choice`: matches `[quality] patterns[0]` at \
         byte 5: No zz.\n\
         warning: `zz-two` is a likely duplicate of `zz-one` (1.000000)\n"
    );
    let status = String::from_utf8(project.run_ok(&["status"]).stdout).unwrap();
    assert!(
        status.contains("quality: 2 limit(s), 0 refusing and 1 warning pattern(s) in [quality]\n"),
        "{status}"
    );

    let broken = table.replace("'zz'", "'(zz'");
    project.write("project.toml", &format!("{header}\n{broken}"));
    let stderr = project.run_refused(&["check"]);
    assert!(
        stderr.contains("project.toml: `[quality] patterns[0].pattern` does not compile"),
        "{stderr}"
    );
    project.run_ok(&["status"]);
    let refusal = client.call_tool_refusal("apply", warned("zz-three"));
    assert_eq!(refusal["kind"], "store_error", "{refusal}");
}

/// A batch whose `[quality]` patterns would do more work than one batch
/// may is refused alike over both surfaces, at the op where the sum
/// passes the bound, and writes nothing.
#[test]
fn a_batch_past_the_pattern_work_is_refused_alike_over_both_surfaces() {
    let project = Project::init();
    let header = project.read("project.toml");
    let costly = "\n[[quality.patterns]]\npattern = '[\\x00-\\x7f]{0,9343}q'\n\
                  fields = [\"reason\"]\nlevel = \"warn\"\nmessage = \"Costly.\"\n";
    project.write("project.toml", &format!("{header}\n[quality]\n{costly}"));
    let ops: Vec<Value> = (0..8)
        .map(|index| {
            let mut op = record(&format!("costly-{index}"), "project", "A rule");
            op["reason"] = json!(format!("{} done.", "x".repeat(20_000)));
            op
        })
        .collect();
    let batch = json!({ "origin": "row 1", "ops": ops });
    let before = project.graph_files();
    let (mut client, _) = McpClient::connect(&project);

    let refusal = client.call_tool_refusal("apply", batch.clone());
    let output = project.apply(&batch, &[]);

    assert_eq!(
        (&refusal["kind"], &refusal["rule"], &refusal["op"]["index"]),
        (&json!("batch_refused"), &json!("pattern_work"), &json!(5)),
        "{refusal}"
    );
    let message = refusal["message"].as_str().unwrap();
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        format!("error: {message}\n")
    );
    assert_eq!(project.graph_files(), before);
}

/// Every refusal of a twin is the tool's refusal JSON on stdout under
/// `--json`, a failed discovery's included, as the MCP call answers it.
#[test]
fn a_twin_refuses_a_store_it_cannot_find_as_the_call_does() {
    let batch = json!({ "ops": [{ "op": "remove", "name": "gone" }] });
    /// What makes the project's directory hold no store it can read.
    type Breaks = fn(&Project);
    let cases: &[(&str, Breaks, &str)] = &[
        ("no store", |_| {}, "no_project"),
        #[cfg(unix)]
        (
            "dangling symlink",
            |project| std::os::unix::fs::symlink("gone", project.graph()).unwrap(),
            "store_error",
        ),
        (
            ".trurlic a file",
            |project| std::fs::write(project.graph(), "").unwrap(),
            "store_error",
        ),
        (
            "old format",
            |project| {
                std::fs::create_dir_all(project.graph()).unwrap();
                project.write("project.toml", "version = \"0.3.0\"\nname = \"old\"\n");
            },
            "unsupported_format",
        ),
    ];
    for &(case, make, kind) in cases {
        let project = Project::without_store();
        make(&project);
        let (mut client, _) = McpClient::connect(&project);
        let file = project.path().join("batch.json");
        std::fs::write(&file, batch.to_string()).unwrap();
        for (tool, arguments, args) in [
            ("status", json!({}), vec!["status", "--json"]),
            (
                "apply",
                batch.clone(),
                vec!["apply", file.to_str().unwrap(), "--json"],
            ),
        ] {
            let call = client.call_tool_refusal(tool, arguments);
            let output = project.command().args(&args).output().unwrap();
            assert_eq!(output.status.code(), Some(1), "{case}: {output:?}");
            let refusal: Value = serde_json::from_slice(&output.stdout)
                .unwrap_or_else(|e| panic!("{case}: {tool}: {e}: {output:?}"));
            assert_eq!(refusal, json!({ "error": call }), "{case}: {tool}");
            assert_eq!(refusal["error"]["kind"], kind, "{case}: {tool}");
        }
    }
}

/// A batch file that cannot be read holds no batch: the refusal is the
/// document's, the kind a batch over its size gets.
#[test]
fn a_batch_file_that_cannot_be_read_is_a_bad_document() {
    let project = Project::init();
    for missing in ["absent.json", "."] {
        let output = project
            .command()
            .args(["apply", missing, "--json"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        let refusal: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(refusal["error"]["kind"], "invalid_argument", "{refusal}");
        let message = refusal["error"]["message"].as_str().unwrap();
        assert!(
            message.starts_with(&format!("{missing}: cannot read the batch")),
            "{message}"
        );
    }
}
