//! The MCP server over real stdio: handshake and version negotiation, the
//! answer to each malformed line, the tool catalogue, one call of every
//! tool it lists, a hand edit seen by the next call, and a server started
//! with no store or an old one.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use crate::golden::assert_golden;
use crate::harness::{McpClient, Project, decision};

#[test]
fn initialize_reports_protocol_and_server() {
    let project = Project::init();
    let (_client, handshake) = McpClient::connect(&project);

    assert_eq!(handshake["protocolVersion"], "2025-11-25");
    assert_eq!(handshake["serverInfo"]["name"], "trurlic");
    assert_eq!(
        handshake["serverInfo"]["version"],
        env!("CARGO_PKG_VERSION")
    );
    assert!(
        handshake["capabilities"]["tools"].is_object(),
        "{handshake}"
    );
}

#[test]
fn initialize_negotiates_the_protocol_version() {
    let project = Project::init();
    let cases = [
        ("2025-11-25", "2025-11-25"),
        ("2025-06-18", "2025-06-18"),
        ("2025-03-26", "2025-11-25"),
        ("2024-11-05", "2025-11-25"),
        ("2999-01-01", "2025-11-25"),
        ("", "2025-11-25"),
    ];
    for (requested, answered) in cases {
        let mut client = McpClient::spawn(&project);
        let handshake = client.request(
            "initialize",
            json!({ "protocolVersion": requested, "capabilities": {} }),
        );
        assert_eq!(handshake["protocolVersion"], answered, "{requested}");
        assert!(
            handshake["capabilities"]["tools"].is_object(),
            "{requested}"
        );
    }
}

/// Each line that is no request is answered once, with the `id` it holds
/// when that is a string or an integer and the line has a `method`, and
/// the session goes on. A line without one would be a response, whose
/// `id` is the server's own: its error carries `null`.
#[test]
fn a_malformed_line_is_answered_once_and_the_session_goes_on() {
    let project = Project::init();
    let (mut client, _) = McpClient::connect(&project);
    let cases = [
        (r#"{"jsonrpc":"2.0","id":41}"#, Value::Null, -32600),
        (
            r#"{"jsonrpc":"1.0","id":42,"result":{}}"#,
            Value::Null,
            -32600,
        ),
        (
            r#"{"jsonrpc":"1.0","id":"x","method":"ping"}"#,
            json!("x"),
            -32600,
        ),
        (
            r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#,
            Value::Null,
            -32600,
        ),
        (
            r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#,
            Value::Null,
            -32600,
        ),
        ("{not json", Value::Null, -32700),
    ];
    for (line, id, code) in cases {
        client.send_line(line);
        let answer = client.receive();
        assert_eq!(answer["id"], id, "{line}: {answer}");
        assert_eq!(answer["error"]["code"], code, "{line}: {answer}");
    }
    assert_eq!(client.request("ping", json!({})), json!({}));
}

/// A line over the 10 MiB limit is read to its end and dropped: one error
/// answers it, and the next line is read as a message of its own.
#[test]
fn an_oversize_line_is_discarded_whole_and_answered_once() {
    let project = Project::init();
    let (mut client, _) = McpClient::connect(&project);
    let padding = "x".repeat(10 * 1024 * 1024 + 1);
    client.send_line(&format!(
        r#"{{"jsonrpc":"2.0","id":5,"method":"{padding}"}}"#
    ));

    let answer = client.receive();
    assert_eq!(answer["id"], Value::Null, "{answer}");
    assert_eq!(answer["error"]["code"], -32600, "{answer}");
    assert_eq!(client.request("ping", json!({})), json!({}));
}

#[test]
fn an_unknown_tool_is_a_protocol_error() {
    let project = Project::init();
    let (mut client, _) = McpClient::connect(&project);
    let response = client.exchange("tools/call", json!({ "name": "advance", "arguments": {} }));
    assert!(response.get("result").is_none(), "{response}");
    assert_eq!(response["error"]["code"], -32602, "{response}");
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("advance"),
        "{response}"
    );
}

/// One call of each listed tool.
fn every_tool() -> [(&'static str, Value); 4] {
    [
        (
            "apply",
            json!({ "ops": [{ "op": "remove", "name": "gone" }] }),
        ),
        ("status", json!({})),
        ("get_context", json!({ "component": "project" })),
        ("get_decisions_for_file", json!({ "file": "src" })),
    ]
}

/// With no `.trurlic/`, the server starts and lists its tools, and every
/// call is refused naming `trurlic init`; once the store exists, the next
/// call is served without a restart.
#[test]
fn a_server_without_a_store_lists_its_tools_and_names_init() {
    let project = Project::without_store();
    let (mut client, _) = McpClient::connect(&project);

    let listed = client.request("tools/list", json!({}));
    assert_eq!(listed["tools"].as_array().unwrap().len(), 4, "{listed}");
    for (tool, arguments) in every_tool() {
        let refusal = client.call_tool_refusal(tool, arguments);
        assert_eq!(refusal["kind"], "no_project", "{tool}: {refusal}");
        let message = refusal["message"].as_str().unwrap();
        assert!(message.contains("trurlic init"), "{tool}: {message}");
    }

    project.run_ok(&["init", "--placement", "shared"]);
    let context = client.call_tool("get_context", json!({ "component": "project" }));
    assert_eq!(context["status"], "not_covered", "{context}");
}

/// A server over a store whose `project.toml` begins with `version_line`
/// in place of the current version: `tools/list` answers, and every tool is
/// refused as `unsupported_format` with a message naming `named`.
fn assert_format_refused(version_line: &str, named: &str) {
    let project = Project::init();
    let current = format!("format_version = \"{}\"\n", trurlic::store::FORMAT_VERSION);
    let text = project.read("project.toml");
    assert!(text.starts_with(&current), "{text}");
    project.write("project.toml", &text.replacen(&current, version_line, 1));

    let (mut client, _) = McpClient::connect(&project);
    let listed = client.request("tools/list", json!({}));
    assert_eq!(listed["tools"].as_array().unwrap().len(), 4, "{listed}");
    for (tool, arguments) in every_tool() {
        let refusal = client.call_tool_refusal(tool, arguments);
        assert_eq!(refusal["kind"], "unsupported_format", "{tool}: {refusal}");
        let message = refusal["message"].as_str().unwrap();
        assert!(message.contains(named), "{tool}: {message}");
    }
}

#[test]
fn a_server_over_an_old_store_lists_its_tools_and_names_migrate() {
    // The manifest as format 0.4.0 wrote it.
    assert_format_refused("trurlic_version = \"0.4.0\"\n", "trurlic migrate");
}

#[test]
fn a_server_over_an_unparseable_version_lists_its_tools_and_names_it() {
    assert_format_refused(
        "format_version = \"0.4.one\"\n",
        "format version `0.4.one` is not MAJOR.MINOR.PATCH",
    );
}

#[test]
fn tools_list_matches_golden() {
    let project = Project::init();
    let (mut client, _) = McpClient::connect(&project);

    let list = client.request("tools/list", json!({}));
    let mut rendered = serde_json::to_string_pretty(&list).unwrap();
    rendered.push('\n');
    assert_golden("tools_list.json", &rendered);
}

/// Before `initialize`, `tools/list` and `tools/call` are refused and
/// `ping` is answered.
#[test]
fn tools_are_refused_before_initialize() {
    let project = Project::init();
    let mut client = McpClient::spawn(&project);

    let listed = client.exchange("tools/list", json!({}));
    let called = client.exchange(
        "tools/call",
        json!({ "name": "get_context", "arguments": { "component": "project" } }),
    );
    for response in [listed, called] {
        assert!(response.get("result").is_none(), "{response}");
        assert_eq!(response["error"]["code"], -32600, "{response}");
    }
    assert_eq!(client.request("ping", json!({})), json!({}));
}

/// The server keeps no graph between calls: node files edited and added by
/// hand between two calls are what the second call answers from.
#[test]
fn the_next_call_sees_a_hand_edit() {
    let project = Project::init();
    project.write("components/auth.toml", "description = \"Auth\"\n");
    project.write(
        "decisions/sign-tokens.toml",
        &decision("auth", "Sign tokens", ""),
    );
    let (mut client, _) = McpClient::connect(&project);
    let before = client.call_tool("get_context", json!({ "component": "auth" }));
    assert_eq!(before["decisions"][0]["choice"], "Sign tokens", "{before}");

    let edited = project
        .read("decisions/sign-tokens.toml")
        .replace("Sign tokens", "Sign tokens with Ed25519");
    project.write("decisions/sign-tokens.toml", &edited);
    project.write("components/billing.toml", "description = \"Invoices\"\n");
    project.write(
        "components/auth.toml",
        "description = \"Auth\"\nconnects_to = [\"billing\"]\n",
    );

    let after = client.call_tool("get_context", json!({ "component": "auth" }));
    assert_eq!(
        after["decisions"][0]["choice"], "Sign tokens with Ed25519",
        "{after}"
    );
    assert_eq!(
        after["component"]["connects_to"],
        json!(["billing"]),
        "{after}"
    );
}

/// Every tool in `tools/list` is called once and succeeds. A tool added to
/// the catalogue without a call here fails the final assertion.
#[test]
fn every_listed_tool_round_trips() {
    let project = Project::init();
    project.write(
        "components/auth.toml",
        "description = \"Sign-in and tokens\"\n",
    );
    let anchor = "anchors = [{ file = \"src/auth.rs\", symbol = \"issue_token\" }]\n";
    project.write(
        "decisions/use-jwt.toml",
        &decision("auth", "Use JWT access tokens", anchor),
    );
    let (mut client, _) = McpClient::connect(&project);

    let listed: BTreeSet<String> = client.request("tools/list", json!({}))["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect();

    let context = client.call_tool("get_context", json!({ "component": "auth" }));
    assert!(
        context["brief"].as_str().unwrap().contains("Use JWT"),
        "{context}"
    );
    let for_file = client.call_tool("get_decisions_for_file", json!({ "file": "src/auth.rs" }));
    assert_eq!(for_file["decisions"][0]["name"], "use-jwt", "{for_file}");
    let applied = client.call_tool(
        "apply",
        json!({ "ops": [{ "op": "revise", "name": "use-jwt", "add_tags": ["tokens"] }] }),
    );
    assert_eq!(applied["ops"][0]["changed"], json!(["tags"]), "{applied}");
    let status = client.call_tool("status", json!({}));
    assert_eq!(status["counts"]["decisions"], 1, "{status}");

    let called: BTreeSet<String> = ["apply", "status", "get_context", "get_decisions_for_file"]
        .map(String::from)
        .into();
    assert_eq!(called, listed, "tools called vs tools listed");
}
