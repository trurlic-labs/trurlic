//! The MCP server over real stdio: handshake and version negotiation, the
//! answer to each malformed line, the tool catalogue, one call of every
//! tool it lists, a hand edit seen by the next call, and a server started
//! with no store or an old one.

use std::collections::BTreeSet;
use std::fs;

use serde_json::{Value, json};

use crate::golden::assert_golden;
use crate::harness::{McpClient, Project};

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
        ("2024-11-05", "2025-11-25"),
        ("2999-01-01", "2025-11-25"),
    ];
    for (requested, answered) in cases {
        let mut client = McpClient::spawn(&project);
        let handshake = client.request(
            "initialize",
            json!({ "protocolVersion": requested, "capabilities": {} }),
        );
        assert_eq!(handshake["protocolVersion"], answered, "{requested}");
    }
}

/// Each line that is no request is answered once, with the `id` it holds
/// when that is a string or an integer, and the session goes on.
#[test]
fn a_malformed_line_is_answered_once_and_the_session_goes_on() {
    let project = Project::init();
    let (mut client, _) = McpClient::connect(&project);
    let cases = [
        (r#"{"jsonrpc":"2.0","id":41}"#, json!(41), -32600),
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

/// With no `.trurlic/`, the server starts and lists its tools, and every
/// call is refused naming `trurlic init`; once the store exists, the next
/// call is served without a restart.
#[test]
fn a_server_without_a_store_lists_its_tools_and_names_init() {
    let project = Project::without_store();
    let (mut client, _) = McpClient::connect(&project);

    let listed = client.request("tools/list", json!({}));
    assert_eq!(listed["tools"].as_array().unwrap().len(), 13, "{listed}");
    for tool in ["get_architecture", "add_component"] {
        let refusal = client.call_tool_refusal(tool, json!({ "name": "auth" }));
        assert_eq!(refusal["kind"], "no_project", "{tool}: {refusal}");
        let message = refusal["message"].as_str().unwrap();
        assert!(message.contains("trurlic init"), "{tool}: {message}");
    }

    project.run_ok(&["init"]);
    let architecture = client.call_tool("get_architecture", json!({}));
    assert!(architecture["components"].is_array(), "{architecture}");
}

/// A store in an older format: the server starts and lists its tools, and
/// every call is refused naming `trurlic migrate`.
#[test]
fn a_server_over_an_old_store_lists_its_tools_and_names_migrate() {
    let project = Project::init();
    let manifest = project.path().join(".trurlic/project.toml");
    let current = format!("\"{}\"", trurlic::store::FORMAT_VERSION);
    let text = fs::read_to_string(&manifest).unwrap();
    assert!(text.contains(&current), "{text}");
    fs::write(&manifest, text.replace(&current, "\"0.1.0\"")).unwrap();

    let (mut client, _) = McpClient::connect(&project);
    let listed = client.request("tools/list", json!({}));
    assert_eq!(listed["tools"].as_array().unwrap().len(), 13, "{listed}");
    for tool in ["get_architecture", "add_component"] {
        let refusal = client.call_tool_refusal(tool, json!({ "name": "auth" }));
        assert_eq!(refusal["kind"], "unsupported_format", "{tool}: {refusal}");
        let message = refusal["message"].as_str().unwrap();
        assert!(message.contains("trurlic migrate"), "{tool}: {message}");
    }
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

#[test]
fn tools_are_refused_before_initialize() {
    let project = Project::init();
    let mut client = McpClient::spawn(&project);

    let response = client.exchange("tools/list", json!({}));
    assert!(response.get("result").is_none(), "{response}");
    assert_eq!(response["error"]["code"], -32600, "{response}");
}

/// The server keeps no graph between calls: node files edited and added by
/// hand between two calls are what the second call answers from.
#[test]
fn the_next_call_sees_a_hand_edit() {
    let project = Project::init();
    project.run_ok(&["add", "component", "auth"]);
    project.run_ok(&[
        "decide",
        "auth",
        "--choice",
        "Sign tokens",
        "--reason",
        "Tokens are verified without a session store",
    ]);
    let (mut client, _) = McpClient::connect(&project);
    let before = client.call_tool("get_decision_history", json!({ "name": "sign-tokens" }));
    assert_eq!(before["current"]["choice"], "Sign tokens", "{before}");

    let store = project.path().join(".trurlic");
    let decision = store.join("decisions/sign-tokens.toml");
    let edited = fs::read_to_string(&decision).unwrap().replace(
        "choice = \"Sign tokens\"",
        "choice = \"Sign tokens with Ed25519\"",
    );
    fs::write(&decision, edited).unwrap();
    fs::write(
        store.join("components/billing.toml"),
        "[component]\nname = \"billing\"\ndescription = \"Invoices\"\n",
    )
    .unwrap();

    let after = client.call_tool("get_decision_history", json!({ "name": "sign-tokens" }));
    assert_eq!(
        after["current"]["choice"], "Sign tokens with Ed25519",
        "{after}"
    );
    let architecture = client.call_tool("get_architecture", json!({}));
    assert_eq!(
        architecture["components"][0]["name"], "auth",
        "{architecture}"
    );
    assert_eq!(
        architecture["components"][1]["name"], "billing",
        "{architecture}"
    );
}

/// Every tool in `tools/list` is called once and succeeds. A tool added to
/// the catalogue without a call here fails the final assertion.
#[test]
fn every_listed_tool_round_trips() {
    let project = Project::init();
    let (mut client, _) = McpClient::connect(&project);

    let listed: BTreeSet<String> = client.request("tools/list", json!({}))["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect();

    let mut session = Session {
        client: &mut client,
        called: BTreeSet::new(),
    };
    session.exercise_writes();
    session.exercise_reads();
    session.exercise_removal();

    assert_eq!(listed.len(), 13, "{listed:?}");
    assert_eq!(session.called, listed, "tools called vs tools listed");
}

struct Session<'a> {
    client: &'a mut McpClient,
    called: BTreeSet<String>,
}

const JWT: &str = "use-jwt-access-tokens";
const ROTATION: &str = "rotate-signing-keys-daily";
const AUDIT: &str = "log-every-failed-login";
const PATTERN: &str = "token-hygiene";

impl Session<'_> {
    fn call(&mut self, name: &str, arguments: Value) -> Value {
        self.called.insert(name.to_owned());
        self.client.call_tool(name, arguments)
    }

    fn decide(&mut self, choice: &str, reason: &str, code_refs: Value) -> String {
        let recorded = self.call(
            "record_decision",
            json!({
                "component": "auth",
                "choice": choice,
                "reason": reason,
                "attribution": "user",
                "alternatives": ["Do nothing: rejected: the test needs a decision"],
                "code_refs": code_refs,
            }),
        );
        recorded["name"].as_str().unwrap().to_owned()
    }

    fn exercise_writes(&mut self) {
        let auth = self.call(
            "add_component",
            json!({ "name": "auth", "description": "Sign-in and tokens" }),
        );
        assert_eq!(auth["name"], "auth");
        self.call("add_component", json!({ "name": "storage" }));
        self.call("add_connection", json!({ "from": "auth", "to": "storage" }));

        let jwt = self.decide(
            "Use JWT access tokens",
            "Stateless verification keeps auth off the storage hot path",
            json!([{ "file": "src/auth.rs", "symbol": "issue_token" }]),
        );
        assert_eq!(jwt, JWT);
        let rotation = self.decide(
            "Rotate signing keys daily",
            "Bounds the damage of a leaked signing key to one day",
            json!([]),
        );
        assert_eq!(rotation, ROTATION);
        let audit = self.decide(
            "Log every failed login",
            "Brute-force attempts must be visible to operators",
            json!([]),
        );
        assert_eq!(audit, AUDIT);

        let pattern = self.call(
            "record_pattern",
            json!({
                "name": "Token hygiene",
                "description": "Tokens are short-lived and their keys rotate",
                "decisions": [JWT, ROTATION],
            }),
        );
        assert_eq!(pattern["name"], PATTERN);
        self.call(
            "update_decision",
            json!({
                "name": JWT,
                "mode": "revise",
                "reason": "Stateless verification; revocation through short expiry",
            }),
        );
    }

    fn exercise_reads(&mut self) {
        let history = self.call("get_decision_history", json!({ "name": JWT }));
        assert_eq!(history["revision_count"], 1, "{history}");

        let context = self.call("get_context", json!({ "component": "auth" }));
        assert!(context["brief"].as_str().unwrap().contains("Use JWT"));

        let architecture = self.call("get_architecture", json!({}));
        assert!(architecture.to_string().contains("storage"));

        self.call(
            "check_pattern",
            json!({ "description": "JWT token rotation" }),
        );

        let for_file = self.call("get_decisions_for_file", json!({ "file": "src/auth.rs" }));
        assert!(for_file.to_string().contains(JWT), "{for_file}");

        let verdicts = self.call(
            "verify_against_decisions",
            json!({ "component": "auth", "changed_files": ["src/auth.rs"] }),
        );
        assert!(verdicts.to_string().contains(JWT), "{verdicts}");
    }

    fn exercise_removal(&mut self) {
        self.call("remove_decision", json!({ "name": AUDIT }));
        let context = self.call("get_context", json!({ "component": "auth" }));
        assert!(!context.to_string().contains(AUDIT), "{context}");

        self.call("remove_pattern", json!({ "name": PATTERN }));
        let architecture = self.call("get_architecture", json!({}));
        assert!(
            !architecture.to_string().contains(PATTERN),
            "{architecture}"
        );
    }
}
