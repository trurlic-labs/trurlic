//! A decision file that is not valid TOML: every surface that loads the
//! graph reports the error with the path of that file.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::json;

use crate::harness::{McpClient, Project};

/// A project with one decision, and the path of its file.
fn project_with_decision() -> (Project, PathBuf) {
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
    let path = fs::canonicalize(project.path())
        .unwrap()
        .join(".trurlic/decisions/sign-tokens.toml");
    assert!(path.is_file());
    (project, path)
}

fn break_toml(path: &Path) {
    let mut file = OpenOptions::new().append(true).open(path).unwrap();
    writeln!(file, "unclosed = [").unwrap();
}

/// The error line names the file, then says what is wrong with it.
fn assert_names(stderr: &str, path: &Path) {
    let expected = format!("{}: invalid TOML", path.display());
    assert!(stderr.contains(&expected), "{expected:?} not in {stderr:?}");
}

#[test]
fn check_names_the_malformed_file() {
    let (project, path) = project_with_decision();
    break_toml(&path);

    let output = project.command().arg("check").output().unwrap();

    assert!(!output.status.success());
    assert_names(&String::from_utf8_lossy(&output.stderr), &path);
}

/// The file breaks while the server runs: the next call loads it and
/// names it.
#[test]
fn a_tool_call_names_the_malformed_file() {
    let (project, path) = project_with_decision();
    let (mut client, _) = McpClient::connect(&project);
    client.call_tool("get_architecture", json!({}));

    break_toml(&path);

    let message = client.call_tool_error("get_architecture", json!({}));
    assert_names(&message, &path);
}
