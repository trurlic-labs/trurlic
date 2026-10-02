//! The MCP server over real stdio: handshake, tool catalogue, and one call
//! of every tool it lists.

use std::collections::BTreeSet;

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
    session.exercise_workflow();
    session.exercise_removal();

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

    fn exercise_workflow(&mut self) {
        let step = self.call(
            "advance",
            json!({ "component": "auth", "task_type": "feature", "mode": "agent" }),
        );
        assert_eq!(step["component"], "auth", "{step}");

        let prompt = self.call(
            "get_step_prompt",
            json!({ "component": "auth", "step": "verify_constraints", "mode": "agent" }),
        );
        assert!(prompt["system_instructions"].is_string(), "{prompt}");
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
