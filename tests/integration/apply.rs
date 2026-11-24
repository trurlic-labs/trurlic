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
    let missing = project.run_refused(&["apply", "absent.json"]);
    assert!(missing.contains("absent.json"), "{missing}");
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
    let said = |name: &str| {
        format!(
            "ops[0] (record `{name}`), `choice`: matches `[quality] patterns[0]` at byte 5 (`zz`): No zz."
        )
    };
    assert_eq!(
        result["warnings"],
        json!([{
            "op": { "index": 0, "op": "record", "name": "zz-one" },
            "field": "ops[0].choice", "rule": "pattern", "message": said("zz-one"),
        }])
    );
    let output = project.apply(&warned("zz-two"), &[]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        format!("warning: {}\n", said("zz-two"))
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
