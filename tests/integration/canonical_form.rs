//! A hand edit is a legal write: a node file in any TOML layout loads,
//! `check` names each file not in canonical form, and `trurlic fmt`
//! rewrites exactly those files, after which a second run has nothing to do.

use std::fs;

use serde_json::json;

use crate::harness::{McpClient, Project, decision};

const AUTH_EDITED: &str =
    "connects_to = [\"store\", \"gateway\", \"store\"]\ndescription = 'Auth'\n";

const AUTH_CANONICAL: &str =
    "description = \"Auth\"\nconnects_to = [\n    \"gateway\",\n    \"store\",\n]\n";

fn project_with_hand_edits() -> Project {
    let project = Project::init();
    project.write("components/auth.toml", AUTH_EDITED);
    for name in ["gateway", "store"] {
        project.write(&format!("components/{name}.toml"), "description = \"\"\n");
    }
    let extra =
        "tags = [\"zeta\", \"alpha\"]\nanchors = [{ symbol = \"sign\", file = \"src/auth.rs\" }]\n";
    project.write(
        "decisions/sign-tokens.toml",
        &decision("auth", "Sign tokens", extra),
    );
    project
}

#[test]
fn a_hand_edited_non_canonical_file_loads_check_flags_it_and_fmt_fixes_it() {
    let project = project_with_hand_edits();

    let (mut client, _) = McpClient::connect(&project);
    let context = client.call_tool("get_context", json!({ "component": "auth" }));
    assert_eq!(
        context["component"]["connects_to"],
        json!(["gateway", "store"])
    );
    assert_eq!(context["decisions"][0]["anchors"][0]["symbol"], "sign");

    let check = project.command().arg("check").output().unwrap();
    assert!(check.status.success(), "a non-canonical file is no error");
    let stderr = String::from_utf8(check.stderr).unwrap();
    assert_eq!(
        stderr,
        "  warning: components/auth.toml is not in canonical form; run `trurlic fmt`\n  \
         warning: decisions/sign-tokens.toml is not in canonical form; run `trurlic fmt`\n"
    );

    let untouched = fs::metadata(project.node("components/store.toml"))
        .unwrap()
        .modified()
        .unwrap();
    let fmt = String::from_utf8(project.run_ok(&["fmt"]).stdout).unwrap();
    assert_eq!(
        fmt,
        "formatted components/auth.toml\nformatted decisions/sign-tokens.toml\n"
    );
    assert_eq!(project.read("components/auth.toml"), AUTH_CANONICAL);
    assert_eq!(
        project.read("decisions/sign-tokens.toml"),
        "component = \"auth\"\nchoice = \"Sign tokens\"\n\
         reason = \"Recorded so the test has a decision to read\"\n\
         tags = [\n    \"alpha\",\n    \"zeta\",\n]\nstatus = \"accepted\"\n\
         created = \"2025-06-01T10:00:00Z\"\n\
         anchors = [\n    { file = \"src/auth.rs\", symbol = \"sign\" },\n]\n"
    );
    let modified = fs::metadata(project.node("components/store.toml"))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(modified, untouched, "a canonical file is not rewritten");

    let check = String::from_utf8(project.run_ok(&["check"]).stdout).unwrap();
    assert_eq!(check, ".trurlic/ is consistent\n");
    let after = client.call_tool("get_context", json!({ "component": "auth" }));
    assert_eq!(after, context, "formatting changes no value");
}

/// `fmt` is a fixpoint: a second run finds every file canonical and writes
/// none.
#[test]
fn fmt_twice_changes_nothing() {
    let project = project_with_hand_edits();
    project.run_ok(&["fmt"]);
    let before: Vec<String> = [
        "components/auth.toml",
        "decisions/sign-tokens.toml",
        "project.toml",
    ]
    .iter()
    .map(|file| project.read(file))
    .collect();

    let again = String::from_utf8(project.run_ok(&["fmt"]).stdout).unwrap();

    assert_eq!(again, ".trurlic/ is in canonical form\n");
    let after: Vec<String> = [
        "components/auth.toml",
        "decisions/sign-tokens.toml",
        "project.toml",
    ]
    .iter()
    .map(|file| project.read(file))
    .collect();
    assert_eq!(after, before);
}

/// `fmt` rewrites a graph with issues too: formatting adds none.
#[test]
fn fmt_formats_a_graph_that_has_issues() {
    let project = project_with_hand_edits();
    project.write(
        "decisions/orphan.toml",
        &decision("gone", "Keep the old API", "depends_on = [\"nowhere\"]\n"),
    );

    project.run_ok(&["fmt"]);

    assert_eq!(project.read("components/auth.toml"), AUTH_CANONICAL);
    let stderr = project.run_refused(&["check"]);
    assert!(
        stderr.contains("component `gone` does not exist"),
        "{stderr}"
    );
    assert!(!stderr.contains("canonical form"), "{stderr}");
}
