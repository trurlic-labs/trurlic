//! A decision file that is not valid TOML: every surface that loads the
//! graph reports the error with the path of that file.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

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

#[test]
fn serve_names_the_malformed_file() {
    let (project, path) = project_with_decision();
    break_toml(&path);

    let output = project.command().arg("serve").output().unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty(), "stdout carries only JSON-RPC");
    assert_names(&String::from_utf8_lossy(&output.stderr), &path);
}

#[test]
fn the_watcher_log_names_the_malformed_file() {
    let (project, path) = project_with_decision();
    let (client, _) = McpClient::connect(&project);
    client.wait_for_diagnostic("file watcher active");

    break_toml(&path);

    let line = client.wait_for_diagnostic("watcher reload");
    assert_names(&line, &path);
}
