//! A graph a hand edit left invalid, which still loads: `check` reports
//! each issue the same way on every run and fails, the reads still answer,
//! and a write that adds no issue of its own still lands.

use serde_json::json;

use crate::harness::{McpClient, Project, decision};

/// `sign-tokens` and `rotate-keys` depend on each other, `expire-sessions`
/// depends into that cycle and on a decision that is gone, `keep-old-api`
/// names a component that is gone, `auth` connects to nothing there, and
/// `billing` shares its name with a decision.
fn invalid_project() -> Project {
    let project = Project::init();
    project.write(
        "components/auth.toml",
        "description = \"\"\nconnects_to = [\"ledger\"]\n",
    );
    project.write("components/billing.toml", "description = \"\"\n");
    for (name, component, depends_on) in [
        ("sign-tokens", "auth", "\"rotate-keys\""),
        ("rotate-keys", "auth", "\"sign-tokens\""),
        ("expire-sessions", "auth", "\"sign-tokens\", \"gone\""),
        ("keep-old-api", "legacy", ""),
        ("billing", "project", ""),
    ] {
        let extra = format!("depends_on = [{depends_on}]\n");
        project.write(
            &format!("decisions/{name}.toml"),
            &decision(component, name, &extra),
        );
    }
    project
}

/// Each run is a new process.
#[test]
fn check_reports_each_issue_once_and_identically_on_every_run() {
    let project = invalid_project();
    let runs: Vec<String> = (0..5).map(|_| project.run_refused(&["check"])).collect();

    assert!(runs.windows(2).all(|pair| pair[0] == pair[1]));
    let errors: Vec<&str> = runs[0]
        .lines()
        .filter(|line| line.starts_with("  error: "))
        .collect();
    assert_eq!(
        errors,
        [
            "  error: components/billing.toml and decisions/billing.toml share the name \
             `billing`; a name is unique across node kinds",
            "  error: decisions/keep-old-api.toml: component `legacy` does not exist",
            "  error: components/auth.toml: `connects_to` names `ledger`, which is no component",
            "  error: decisions/expire-sessions.toml: `depends_on` names `gone`, which is no decision",
            "  error: depends_on cycle among `rotate-keys`, `sign-tokens`",
        ]
    );
    assert!(
        runs[0].ends_with("error: 5 consistency error(s) found\n"),
        "{}",
        runs[0]
    );
}

#[test]
fn the_reads_answer_over_an_invalid_graph() {
    let project = invalid_project();
    let (mut client, _) = McpClient::connect(&project);

    let context = client.call_tool("get_context", json!({ "component": "auth" }));

    let names: Vec<&str> = context["decisions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["expire-sessions", "rotate-keys", "sign-tokens"]);
    let status = String::from_utf8(project.run_ok(&["status"]).stdout).unwrap();
    assert!(status.contains("issues: 5"), "{status}");
}

/// `fmt` adds no issue, so it lands beside the ones the graph has.
#[test]
fn fmt_lands_beside_the_issues_and_keeps_them() {
    let project = invalid_project();
    let before = project.run_refused(&["check"]);

    project.run_ok(&["fmt"]);

    let after = project.run_refused(&["check"]);
    assert!(!after.contains("canonical form"), "{after}");
    let errors = |report: &str| -> Vec<String> {
        report
            .lines()
            .filter(|line| line.starts_with("  error: "))
            .map(str::to_owned)
            .collect()
    };
    assert_eq!(errors(&after), errors(&before));
    assert!(
        project
            .read("decisions/rotate-keys.toml")
            .contains("depends_on = [\n    \"sign-tokens\",\n]\n")
    );
}
