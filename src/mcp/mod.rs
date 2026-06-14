mod context;
mod protocol;
mod tools;
mod update;
mod watcher;
mod write;

use std::io::{self, Write};
use std::sync::{Arc, LazyLock, RwLock};

use serde_json::Value;

use crate::Result;
use crate::store::{ProjectState, Store};

use protocol::{INVALID_PARAMS, INVALID_REQUEST, METHOD_NOT_FOUND, PARSE_ERROR, Request};

const PROTOCOL_VERSION: &str = "2025-11-25";

// ── Public entry point ────────────────────────────────────────────────────

/// Run the MCP server on stdio.
///
/// `initial_state` is wrapped in `Arc<RwLock<_>>` and shared with a
/// background file watcher thread. The watcher detects external changes
/// to `.trurlic/` (CLI writes, manual edits, git checkout) and reloads
/// state from disk. The write lock is held only for pointer swaps
/// (microseconds) — MCP read queries acquire only a read lock and never
/// block the watcher or other reads.
pub(crate) fn run_server(store: Store, initial_state: ProjectState) -> Result<()> {
    let state = Arc::new(RwLock::new(initial_state));

    // Spawn file watcher. Non-fatal if unavailable (e.g. inotify limit).
    let _watcher = match watcher::spawn(store.root(), state.clone()) {
        Ok(guard) => {
            eprintln!("trurlic: file watcher active");
            Some(guard)
        }
        Err(e) => {
            eprintln!("trurlic: file watcher unavailable: {e}");
            None
        }
    };

    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = stdin.lock();
    let mut writer = stdout.lock();
    let mut initialized = false;

    eprintln!("trurlic: MCP server ready");

    loop {
        match protocol::read_message(&mut reader) {
            Ok(Some(request)) => {
                if let Err(e) = handle(&store, &state, request, &mut initialized, &mut writer) {
                    eprintln!("trurlic: stdout write error: {e}");
                    break;
                }
            }
            Ok(None) => break, // EOF — clean shutdown
            Err(e) => {
                if let Err(we) =
                    protocol::write_error(&mut writer, &Value::Null, PARSE_ERROR, &e.to_string())
                {
                    eprintln!("trurlic: stdout write error: {we}");
                    break;
                }
            }
        }
    }

    eprintln!("trurlic: MCP server stopped");
    Ok(())
}

// ── Request dispatch ──────────────────────────────────────────────────────

/// Dispatch a JSON-RPC request and write the response directly to `writer`.
///
/// Writing inline avoids boxing or enum-wrapping different result types:
/// cold-path responses (initialize, ping, tools/list) write `Value`
/// results via `write_success`, while tool-call responses write the typed
/// `ToolEnvelope` directly — single serialization pass, zero intermediate
/// `Value` allocation.
///
/// Notifications produce no output (the writer is untouched).
fn handle(
    store: &Store,
    state: &Arc<RwLock<ProjectState>>,
    request: Request,
    initialized: &mut bool,
    writer: &mut impl Write,
) -> io::Result<()> {
    // Notifications never receive a response.
    if request.is_notification() {
        return Ok(());
    }

    let id = request.id.unwrap_or(Value::Null);

    // JSON-RPC 2.0 §4.1: jsonrpc MUST be exactly "2.0".
    if request.jsonrpc != "2.0" {
        return protocol::write_error(
            writer,
            &id,
            INVALID_REQUEST,
            &format!(
                "invalid jsonrpc version: expected \"2.0\", got {:?}",
                request.jsonrpc
            ),
        );
    }

    match request.method.as_str() {
        "initialize" => {
            *initialized = true;
            protocol::write_success(writer, &id, &*INITIALIZE_RESULT)
        }
        "ping" => protocol::write_success(writer, &id, &serde_json::json!({})),

        // Gate: tool operations require initialization.
        "tools/list" if *initialized => protocol::write_success(writer, &id, tools::tool_list()),
        "tools/call" if *initialized => match handle_tools_call(store, state, &request.params) {
            Ok(envelope) => protocol::write_success(writer, &id, &envelope),
            Err((code, msg)) => protocol::write_error(writer, &id, code, &msg),
        },
        "tools/list" | "tools/call" => protocol::write_error(
            writer,
            &id,
            INVALID_REQUEST,
            "server not initialized \u{2014} send initialize first",
        ),
        _ => protocol::write_error(
            writer,
            &id,
            METHOD_NOT_FOUND,
            &format!("unknown method: {}", request.method),
        ),
    }
}

/// Static initialize response. Built once on first access — the protocol
/// version and server info are compile-time constants.
static INITIALIZE_RESULT: LazyLock<Value> = LazyLock::new(|| {
    serde_json::json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {
            "tools": {}
        },
        "serverInfo": {
            "name": "trurlic",
            "version": env!("CARGO_PKG_VERSION"),
        }
    })
});

fn handle_tools_call(
    store: &Store,
    state: &Arc<RwLock<ProjectState>>,
    params: &Option<Value>,
) -> std::result::Result<tools::ToolEnvelope, (i32, String)> {
    let params = params
        .as_ref()
        .ok_or_else(|| (INVALID_PARAMS, "missing params in tools/call".to_string()))?;

    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| (INVALID_PARAMS, "missing or invalid tool name".to_string()))?;

    static EMPTY_ARGS: LazyLock<Value> = LazyLock::new(|| serde_json::json!({}));
    let arguments = params.get("arguments").unwrap_or(&EMPTY_ARGS);

    // Write tools need &mut ProjectState; read tools (and unknown names)
    // only need &ProjectState. Acquiring only a read lock for reads avoids
    // blocking the file watcher's state swap and (if the transport ever
    // supports concurrency) other read requests.
    if tools::is_write_tool(name) {
        let mut guard = state.write().unwrap_or_else(|poisoned| {
            eprintln!("trurlic: recovered from poisoned state lock");
            poisoned.into_inner()
        });
        Ok(tools::call_write_tool(store, &mut guard, name, arguments))
    } else {
        let guard = state.read().unwrap_or_else(|poisoned| {
            eprintln!("trurlic: recovered from poisoned state lock");
            poisoned.into_inner()
        });
        Ok(tools::call_read_tool(&guard, name, arguments))
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_request(id: Option<Value>, method: &str, params: Option<Value>) -> Request {
        Request {
            jsonrpc: "2.0".into(),
            id,
            method: method.into(),
            params,
        }
    }

    /// Dispatch a request through `handle`, capturing the wire output.
    /// Returns `None` for notifications (no bytes written), `Some(json)`
    /// for everything else. Panics on I/O error — acceptable in tests.
    fn handle_to_json(
        store: &Store,
        state: &Arc<RwLock<ProjectState>>,
        req: Request,
        initialized: &mut bool,
    ) -> Option<Value> {
        let mut buf = Vec::new();
        handle(store, state, req, initialized, &mut buf).unwrap();
        if buf.is_empty() {
            None
        } else {
            Some(serde_json::from_slice(&buf).unwrap())
        }
    }

    fn empty_state() -> Arc<RwLock<ProjectState>> {
        Arc::new(RwLock::new(crate::store::testing::empty_project_state()))
    }

    fn empty_store() -> (tempfile::TempDir, Store) {
        let tmp = tempfile::TempDir::new().unwrap();
        crate::commands::init(tmp.path()).unwrap();
        let store = Store::discover(tmp.path()).unwrap();
        (tmp, store)
    }

    #[test]
    fn notification_writes_nothing() {
        let (_tmp, store) = empty_store();
        let state = Arc::new(RwLock::new(store.load_state().unwrap()));
        let mut initialized = true;
        let req = make_request(None, "notifications/initialized", None);
        assert!(handle_to_json(&store, &state, req, &mut initialized).is_none());
    }

    #[test]
    fn initialize_returns_capabilities() {
        let (_tmp, store) = empty_store();
        let state = Arc::new(RwLock::new(store.load_state().unwrap()));
        let mut initialized = false;
        let req = make_request(Some(json!(1)), "initialize", None);
        let json = handle_to_json(&store, &state, req, &mut initialized).unwrap();
        assert!(initialized);
        let result = &json["result"];
        assert_eq!(result["protocolVersion"], PROTOCOL_VERSION);
        assert!(result["capabilities"]["tools"].is_object());
        assert_eq!(result["serverInfo"]["name"], "trurlic");
    }

    #[test]
    fn ping_returns_empty_object() {
        let state = empty_state();
        let store = Store::at("/dev/null/.trurlic".into());
        let mut initialized = true;
        let req = make_request(Some(json!(2)), "ping", None);
        let json = handle_to_json(&store, &state, req, &mut initialized).unwrap();
        assert_eq!(json["result"], json!({}));
    }

    #[test]
    fn unknown_method_returns_error() {
        let state = empty_state();
        let store = Store::at("/dev/null/.trurlic".into());
        let mut initialized = true;
        let req = make_request(Some(json!(3)), "bogus/method", None);
        let json = handle_to_json(&store, &state, req, &mut initialized).unwrap();
        assert_eq!(json["error"]["code"], METHOD_NOT_FOUND);
    }

    #[test]
    fn tools_call_missing_params_returns_error() {
        let state = empty_state();
        let store = Store::at("/dev/null/.trurlic".into());
        let mut initialized = true;
        let req = make_request(Some(json!(4)), "tools/call", None);
        let json = handle_to_json(&store, &state, req, &mut initialized).unwrap();
        assert_eq!(json["error"]["code"], INVALID_PARAMS);
    }

    #[test]
    fn tools_call_missing_name_returns_error() {
        let state = empty_state();
        let store = Store::at("/dev/null/.trurlic".into());
        let mut initialized = true;
        let req = make_request(Some(json!(5)), "tools/call", Some(json!({"arguments": {}})));
        let json = handle_to_json(&store, &state, req, &mut initialized).unwrap();
        assert_eq!(json["error"]["code"], INVALID_PARAMS);
    }

    #[test]
    fn tools_list_returns_all_tools() {
        let state = empty_state();
        let store = Store::at("/dev/null/.trurlic".into());
        let mut initialized = true;
        let req = make_request(Some(json!(6)), "tools/list", None);
        let json = handle_to_json(&store, &state, req, &mut initialized).unwrap();
        let tools = json["result"]["tools"].as_array().unwrap();
        assert!(tools.len() >= 3);
    }

    #[test]
    fn tools_call_before_initialize_rejected() {
        let state = empty_state();
        let store = Store::at("/dev/null/.trurlic".into());
        let mut initialized = false;
        let req = make_request(
            Some(json!(7)),
            "tools/call",
            Some(json!({"name": "get_architecture", "arguments": {}})),
        );
        let json = handle_to_json(&store, &state, req, &mut initialized).unwrap();
        assert_eq!(json["error"]["code"], INVALID_REQUEST);
        assert!(
            json["error"]["message"]
                .as_str()
                .unwrap()
                .contains("not initialized")
        );
    }

    #[test]
    fn tools_list_before_initialize_rejected() {
        let state = empty_state();
        let store = Store::at("/dev/null/.trurlic".into());
        let mut initialized = false;
        let req = make_request(Some(json!(8)), "tools/list", None);
        let json = handle_to_json(&store, &state, req, &mut initialized).unwrap();
        assert_eq!(json["error"]["code"], INVALID_REQUEST);
    }

    #[test]
    fn invalid_jsonrpc_version_rejected() {
        let state = empty_state();
        let store = Store::at("/dev/null/.trurlic".into());
        let mut initialized = true;
        let req = Request {
            jsonrpc: "1.0".into(),
            id: Some(json!(9)),
            method: "ping".into(),
            params: None,
        };
        let json = handle_to_json(&store, &state, req, &mut initialized).unwrap();
        assert_eq!(json["error"]["code"], INVALID_REQUEST);
    }

    #[test]
    fn ping_allowed_before_initialize() {
        let state = empty_state();
        let store = Store::at("/dev/null/.trurlic".into());
        let mut initialized = false;
        let req = make_request(Some(json!(10)), "ping", None);
        let json = handle_to_json(&store, &state, req, &mut initialized).unwrap();
        assert_eq!(json["result"], json!({}));
    }

    #[test]
    fn read_tool_does_not_acquire_write_lock() {
        let (_tmp, store) = empty_store();
        let state = Arc::new(RwLock::new(store.load_state().unwrap()));
        let mut initialized = true;

        // Hold a read lock — if handle_tools_call tried to write-lock
        // this would deadlock (single-threaded test) or panic.
        let _read_guard = state.read().unwrap();

        // get_architecture is a read tool — must succeed with the read
        // lock already held by us (RwLock allows multiple readers).
        let req = make_request(
            Some(json!(11)),
            "tools/call",
            Some(json!({"name": "get_architecture", "arguments": {}})),
        );
        let json = handle_to_json(&store, &state, req, &mut initialized).unwrap();
        assert!(json.get("result").is_some(), "read tool should succeed");
    }

    #[test]
    fn tool_result_not_double_escaped() {
        // Verify that the tool response text field contains valid JSON,
        // not double-escaped JSON. This catches regressions where a
        // serialization step escapes the already-serialized payload.
        let (_tmp, store) = empty_store();
        let state = Arc::new(RwLock::new(store.load_state().unwrap()));
        let mut initialized = true;
        let req = make_request(
            Some(json!(12)),
            "tools/call",
            Some(json!({"name": "get_architecture", "arguments": {}})),
        );
        let json = handle_to_json(&store, &state, req, &mut initialized).unwrap();
        let text = json["result"]["content"][0]["text"].as_str().unwrap();
        // text must be parseable JSON (the tool payload)
        let inner: Value = serde_json::from_str(text).unwrap();
        assert!(inner.is_object(), "tool payload should be a JSON object");
    }

    #[test]
    fn responses_are_single_line_json() {
        let state = empty_state();
        let store = Store::at("/dev/null/.trurlic".into());
        let mut initialized = true;

        // Success response
        let mut buf = Vec::new();
        let req = make_request(Some(json!(1)), "ping", None);
        handle(&store, &state, req, &mut initialized, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert_eq!(
            output.lines().count(),
            1,
            "success response must be a single line"
        );
        assert!(output.ends_with('\n'));

        // Error response
        let mut buf = Vec::new();
        let req = make_request(Some(json!(2)), "bogus", None);
        handle(&store, &state, req, &mut initialized, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert_eq!(
            output.lines().count(),
            1,
            "error response must be a single line"
        );
        assert!(output.ends_with('\n'));
    }
}
