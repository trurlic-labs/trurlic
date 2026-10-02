//! `TRURLIC_FAILPOINT` aborts the binary at a named store write site, and
//! the next run starts from a graph `trurlic check` accepts.

use std::fs;
use std::process::ExitStatus;

use serde_json::json;

use crate::harness::{McpClient, Project};

const FAILPOINT: &str = "TRURLIC_FAILPOINT";

fn assert_aborted(status: ExitStatus) {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(6), "expected SIGABRT, got {status}");
    }
    // Exit code 1 is an ordinary error; an abort is neither success nor that.
    assert!(!status.success());
    assert_ne!(status.code(), Some(1), "{status}");
}

/// Run `trurlic <args>` with `spec` armed and assert it aborted there.
fn run_aborting(project: &Project, args: &[&str], spec: &str) {
    let output = project
        .command()
        .args(args)
        .env(FAILPOINT, spec)
        .output()
        .unwrap();
    assert_aborted(output.status);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&format!("failpoint {spec} hit")),
        "{stderr}"
    );
}

/// The next CLI run cleans the interrupted write and finds a consistent graph.
fn assert_recovers(project: &Project) {
    let output = project.run_ok(&["check"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("is consistent"), "{stdout}");
    let leftovers = fs::read_dir(project.path().join(".trurlic/.state/tmp"))
        .unwrap()
        .count();
    assert_eq!(leftovers, 0, "temp files survived the next run");
}

fn graph_lists(project: &Project, name: &str) -> bool {
    let graph = fs::read_to_string(project.path().join(".trurlic/graph.toml")).unwrap();
    graph.contains(&format!("name = \"{name}\""))
}

fn component_file_exists(project: &Project, name: &str) -> bool {
    project
        .path()
        .join(format!(".trurlic/components/{name}.toml"))
        .exists()
}

fn decision_file_exists(project: &Project, name: &str) -> bool {
    project
        .path()
        .join(format!(".trurlic/decisions/{name}.toml"))
        .exists()
}

#[test]
fn staged_abort_commits_nothing() {
    let project = Project::init();

    run_aborting(&project, &["add", "component", "auth"], "commit.staged:1");

    assert!(!component_file_exists(&project, "auth"));
    assert!(!graph_lists(&project, "auth"));
    assert_recovers(&project);

    // The aborted commit had raised the generation; that costs nothing.
    project.run_ok(&["add", "component", "auth"]);
    assert!(graph_lists(&project, "auth"));
}

/// The node file is in place but `graph.toml`, the commit point, is not.
#[test]
fn nodes_renamed_abort_leaves_graph_uncommitted() {
    let project = Project::init();

    run_aborting(
        &project,
        &["add", "component", "auth"],
        "commit.nodes_renamed:1",
    );

    assert!(component_file_exists(&project, "auth"));
    assert!(!graph_lists(&project, "auth"));
    assert_recovers(&project);
}

/// `graph.toml` no longer lists the decision, but its node file, which
/// phase 4 would have deleted, is still on disk.
#[test]
fn graph_renamed_abort_leaves_the_removed_node_file() {
    let project = Project::init();
    project.run_ok(&["add", "component", "auth"]);
    project.run_ok(&[
        "decide",
        "auth",
        "--choice",
        "Use JWT",
        "--reason",
        "Stateless verification",
    ]);
    assert!(graph_lists(&project, "use-jwt"));

    run_aborting(
        &project,
        &["remove", "decision", "use-jwt"],
        "commit.graph_renamed:1",
    );

    assert!(!graph_lists(&project, "use-jwt"));
    assert!(decision_file_exists(&project, "use-jwt"));
    assert_recovers(&project);
}

/// `<n>` counts hits within one process: the first commit passes the site,
/// the second aborts there before renaming anything.
#[test]
fn failpoint_fires_on_the_nth_hit() {
    let project = Project::init();
    let (mut client, _) = McpClient::connect_with_env(&project, &[(FAILPOINT, "commit.staged:2")]);

    client.call_tool("add_component", json!({ "name": "auth" }));
    let response = client.try_exchange(
        "tools/call",
        json!({ "name": "add_component", "arguments": { "name": "storage" } }),
    );

    assert!(response.is_none(), "server answered: {response:?}");
    assert_aborted(client.finish());
    assert!(graph_lists(&project, "auth"));
    assert!(!component_file_exists(&project, "storage"));
}

/// A malformed spec is reported and ignored; it never aborts.
#[test]
fn malformed_failpoint_is_ignored() {
    let project = Project::init();

    let output = project
        .command()
        .args(["add", "component", "auth"])
        .env(FAILPOINT, "commit.nowhere:1")
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", output.status);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ignoring malformed TRURLIC_FAILPOINT"),
        "{stderr}"
    );
    assert!(graph_lists(&project, "auth"));
}
