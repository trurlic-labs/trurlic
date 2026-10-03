//! A decision's choice and reason are checked by the store, so every surface
//! refuses the same text with the same message: MCP `record_decision` and
//! `update_decision`, the map's revise endpoint and `trurlic decide`. A
//! refused write leaves the graph on disk as it was.

use std::fs;
use std::path::Path;

use serde_json::json;

use crate::harness::{MapServer, McpClient, Project};

const CHOICE: &str = "Expire sessions after an hour";
const REASON: &str = "An hour bounds how long a stolen token is useful";

/// The decision every revise targets.
const REVISED: &str = "sign-tokens";

/// Text one field must refuse, the class of fault it carries, and how the
/// refusal begins.
struct Case {
    class: &'static str,
    field: &'static str,
    text: String,
    refusal: String,
}

fn case(class: &'static str, field: &'static str, text: impl Into<String>) -> Case {
    Case {
        class,
        field,
        text: text.into(),
        refusal: format!("`{field}` "),
    }
}

fn reason_with(class: &'static str, inserted: &str) -> Case {
    case(class, "reason", format!("{REASON}{inserted} for the rest"))
}

fn cases() -> Vec<Case> {
    vec![
        case("blank choice", "choice", "   "),
        case("multi-line choice", "choice", "Sign tokens\nwith Ed25519"),
        case("tab in a choice", "choice", "Sign\ttokens"),
        case("choice over 200 bytes", "choice", "c".repeat(201)),
        case("argument tag in a choice", "choice", "Sign tokens</choice>"),
        case("reason under 10 bytes", "reason", "  too short  "),
        reason_with("C0 control", "\u{1B}[31m"),
        reason_with("carriage return", "\r\n"),
        reason_with("DEL", "\u{7F}"),
        reason_with("C1 control", "\u{85}"),
        reason_with("bidi override", "\u{202E}"),
        reason_with("bidi isolate", "\u{2066}"),
        reason_with("zero-width space", "\u{200B}"),
        reason_with("byte order mark", "\u{FEFF}"),
        reason_with("line separator", "\u{2028}"),
        reason_with("paragraph separator", "\u{2029}"),
        reason_with("closing parameter tag", "</parameter>"),
        reason_with("opening parameter tag", "<parameter name=\"tags\">"),
        reason_with("invoke tag", "<invoke name=\"advance\">"),
        reason_with("function_calls tag", "</function_calls>"),
        reason_with("argument tag in a reason", "</alternatives>"),
        Case {
            refusal: "decision `rotate-keys` in [auth] already has this choice".into(),
            ..case("duplicate choice", "choice", "rotate  KEYS")
        },
    ]
}

/// A project whose `auth` component holds `sign-tokens` and `rotate-keys`.
fn project() -> Project {
    let project = Project::init();
    project.run_ok(&["add", "component", "auth"]);
    for choice in ["Sign tokens", "Rotate keys"] {
        project.run_ok(&["decide", "auth", "--choice", choice, "--reason", REASON]);
    }
    project
}

/// Every decision file with its bytes.
fn decisions_on_disk(project: &Path) -> Vec<(String, Vec<u8>)> {
    let mut files: Vec<_> = fs::read_dir(project.join(".trurlic/decisions"))
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            (name, fs::read(&path).unwrap())
        })
        .collect();
    files.sort();
    files
}

/// The message each surface answers `case` with, by surface.
fn refusals(
    project: &Project,
    mcp: &mut McpClient,
    map: &MapServer,
    case: &Case,
) -> [(&'static str, String); 4] {
    let (choice, reason) = match case.field {
        "choice" => (case.text.as_str(), REASON),
        _ => (CHOICE, case.text.as_str()),
    };

    let recorded = mcp.call_tool_error(
        "record_decision",
        json!({
            "component": "auth",
            "choice": choice,
            "reason": reason,
            "attribution": "agent",
        }),
    );
    let revised = mcp.call_tool_error(
        "update_decision",
        json!({ "name": REVISED, "mode": "revise", case.field: case.text }),
    );

    let (status, body) = map.request(
        "PUT",
        &format!("/api/decision/{REVISED}"),
        Some(&json!({ case.field: case.text })),
    );
    assert_eq!(status, 400, "{}: map answered {body}", case.class);
    let mapped = body["error"].as_str().unwrap().to_owned();

    let output = project
        .command()
        .args(["decide", "auth", "--choice", choice, "--reason", reason])
        .output()
        .unwrap();
    assert!(!output.status.success(), "{}: decide succeeded", case.class);
    let stderr = String::from_utf8(output.stderr).unwrap();
    let decided = stderr
        .strip_prefix("error: ")
        .and_then(|message| message.strip_suffix('\n'))
        .unwrap_or_else(|| panic!("{}: unexpected stderr {stderr:?}", case.class))
        .to_owned();

    [
        ("record_decision", recorded),
        ("update_decision", revised),
        ("map", mapped),
        ("decide", decided),
    ]
}

#[test]
fn every_surface_refuses_each_class_with_the_stores_message() {
    let project = project();
    let mut mcp = McpClient::connect(&project).0;
    let map = MapServer::start(&project);
    let before = decisions_on_disk(project.path());

    for case in cases() {
        let refusals = refusals(&project, &mut mcp, &map, &case);

        let (_, expected) = &refusals[0];
        assert!(
            expected.starts_with(&case.refusal),
            "{}: {expected}",
            case.class
        );
        for (surface, message) in &refusals {
            assert_eq!(message, expected, "{}: {surface}", case.class);
        }
    }

    assert_eq!(decisions_on_disk(project.path()), before);
}
