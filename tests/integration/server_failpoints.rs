//! Failpoints inside `trurlic serve`: a panic in a tool call, answered
//! -32603 while the server serves on, and a tool call that sends the client
//! a request of its own and waits for the answer on the same stdio.

use serde_json::{Value, json};

use crate::harness::{McpClient, Project};

const FAILPOINT: &str = "TRURLIC_FAILPOINT";
const SERVER_REQUEST: (&str, &str) = (FAILPOINT, "mcp.server_request:1");

/// The panic hits under the exclusive lock. Unwinding releases it, so the
/// next write takes the lock and commits; nothing is poisoned.
#[test]
fn a_panic_in_a_tool_call_is_answered_internal_error_and_the_server_serves_on() {
    let project = Project::init();
    let (mut client, _) = McpClient::connect_with_env(&project, &[(FAILPOINT, "write.locked:1")]);

    let response = client.exchange(
        "tools/call",
        json!({ "name": "add_component", "arguments": { "name": "auth" } }),
    );
    assert!(response.get("result").is_none(), "{response}");
    assert_eq!(response["error"]["code"], -32603, "{response}");
    let message = response["error"]["message"].as_str().unwrap();
    assert!(message.contains("panicked"), "{message}");

    client.call_tool("add_component", json!({ "name": "storage" }));
    let architecture = client.call_tool("get_architecture", json!({}));
    let components = architecture["components"].as_array().unwrap();
    assert_eq!(components.len(), 1, "{architecture}");
    assert_eq!(components[0]["name"], "storage", "{architecture}");
}

/// A client that declares form elicitation.
fn eliciting_client(project: &Project) -> McpClient {
    McpClient::connect_declaring(project, &[SERVER_REQUEST], json!({ "elicitation": {} })).0
}

/// Send a `tools/call`, and return its id and the request the server
/// sends in its place.
fn call_and_receive_request(client: &mut McpClient) -> (u64, Value) {
    let call = client.next_id();
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": call,
        "method": "tools/call",
        "params": { "name": "get_architecture", "arguments": {} },
    }));
    let request = client.receive();
    assert_eq!(request["method"], "elicitation/create", "{request}");
    assert!(
        request["params"]["requestedSchema"].is_object(),
        "{request}"
    );
    (call, request)
}

/// While the call waits: a ping is answered at once, a notification and a
/// second request wait; the answer completes the call, then the queued
/// request is served.
#[test]
fn a_tool_call_waits_for_the_answer_to_its_request_while_serving_the_line() {
    let project = Project::init();
    let mut client = eliciting_client(&project);
    let (call, request) = call_and_receive_request(&mut client);

    client.send(&json!({ "jsonrpc": "2.0", "id": "ping-1", "method": "ping" }));
    assert_eq!(
        client.receive(),
        json!({ "jsonrpc": "2.0", "id": "ping-1", "result": {} })
    );
    client.notify("notifications/progress");
    let queued = client.next_id();
    client.send(&json!({ "jsonrpc": "2.0", "id": queued, "method": "tools/list" }));

    client.send(&json!({
        "jsonrpc": "2.0",
        "id": request["id"],
        "result": { "action": "accept", "content": { "confirmed": true } },
    }));

    let answered = client.receive();
    assert_eq!(answered["id"], call, "{answered}");
    let text = answered["result"]["content"][0]["text"].as_str().unwrap();
    let payload: Value = serde_json::from_str(text).unwrap();
    assert_eq!(
        payload,
        json!({ "action": "accept", "content": { "confirmed": true } })
    );
    let listed = client.receive();
    assert_eq!(listed["id"], queued, "{listed}");
    assert!(listed["result"]["tools"].is_array(), "{listed}");
}

#[test]
fn a_client_without_elicitation_gets_a_refusal_and_no_request() {
    let project = Project::init();
    let (mut client, _) = McpClient::connect_declaring(&project, &[SERVER_REQUEST], json!({}));

    let refusal = client.call_tool_refusal("get_architecture", json!({}));
    let message = refusal["message"].as_str().unwrap();
    assert!(message.contains("elicitation"), "{message}");
}

#[test]
fn an_error_answer_is_a_refusal_naming_its_code() {
    let project = Project::init();
    let mut client = eliciting_client(&project);
    let (call, request) = call_and_receive_request(&mut client);

    client.send(&json!({
        "jsonrpc": "2.0",
        "id": request["id"],
        "error": { "code": -32602, "message": "form mode not supported" },
    }));

    let answered = client.receive();
    assert_eq!(answered["id"], call);
    assert_eq!(answered["result"]["isError"], true, "{answered}");
    let text = answered["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("-32602"), "{text}");
}

/// A cancelled call ends its wait and gets no response.
#[test]
fn a_cancelled_call_gets_no_response() {
    let project = Project::init();
    let mut client = eliciting_client(&project);
    let (call, _) = call_and_receive_request(&mut client);

    client.send(&json!({
        "jsonrpc": "2.0",
        "method": "notifications/cancelled",
        "params": { "requestId": call, "reason": "the user closed the form" },
    }));

    assert_eq!(client.request("ping", json!({})), json!({}));
}

/// stdin closing during the wait ends the call and the server, cleanly.
#[test]
fn stdin_closing_during_the_wait_stops_the_server_cleanly() {
    let project = Project::init();
    let mut client = eliciting_client(&project);
    call_and_receive_request(&mut client);

    let status = client.finish();
    assert!(status.success(), "{status}");
}
