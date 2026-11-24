//! Two git branches of one tracked graph, each adding a decision to the
//! same component that depends on the same decision, and each adding an
//! edge to the same existing decision, merge without a conflict and pass
//! `check`. A new decision's edges live in its own new file, and an edge
//! added to an existing set is one line of a sorted list, so the branches
//! touch different lines.

use std::path::Path;
use std::process::{Command, Output};

use crate::harness::{Project, decision};

/// `git <args>` in `dir`, isolated from the user's and the system's git
/// configuration, with a fixed identity.
fn git(dir: &Path, args: &[&str]) -> Output {
    let output = Command::new("git")
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_WORK_TREE")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args([
            "-c",
            "user.name=trurlic-tests",
            "-c",
            "user.email=tests@trurlic.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn commit_all(dir: &Path, message: &str) {
    git(dir, &["add", "--all"]);
    git(dir, &["commit", "--quiet", "--no-verify", "-m", message]);
}

/// `auth` with `sign-tokens`, and `rotate-keys`, which depends on it.
fn base(project: &Project) {
    project.write("components/auth.toml", "description = \"Sign-in\"\n");
    project.write(
        "decisions/sign-tokens.toml",
        &decision("auth", "Sign tokens", ""),
    );
    project.write(
        "decisions/rotate-keys.toml",
        &decision("auth", "Rotate keys", "depends_on = [\"sign-tokens\"]\n"),
    );
    project.run_ok(&["fmt"]);
}

/// A new decision of `auth` that depends on `sign-tokens`, named in
/// `rotate-keys`' dependencies too, through the CLI's canonical rewrite.
fn add_on_branch(project: &Project, branch: &str, name: &str, choice: &str) {
    let dir = project.path();
    git(dir, &["checkout", "--quiet", "-b", branch, "main"]);
    project.write(
        &format!("decisions/{name}.toml"),
        &decision("auth", choice, "depends_on = [\"sign-tokens\"]\n"),
    );
    let rotate = project.read("decisions/rotate-keys.toml").replace(
        "depends_on = [\n",
        &format!("depends_on = [\n    \"{name}\",\n"),
    );
    project.write("decisions/rotate-keys.toml", &rotate);
    project.run_ok(&["fmt"]);
    commit_all(dir, &format!("add {name}"));
}

#[test]
fn two_branches_that_each_add_a_decision_and_an_edge_merge_and_pass_check() {
    let project = Project::init();
    let dir = project.path();
    git(dir, &["init", "--quiet", "--initial-branch=main"]);
    base(&project);
    commit_all(dir, "base");

    // One name sorts before the existing dependency and one after it, as
    // two unrelated names usually do.
    add_on_branch(&project, "one", "expire-sessions", "Expire sessions");
    add_on_branch(&project, "two", "verify-audience", "Verify the audience");

    git(dir, &["checkout", "--quiet", "main"]);
    git(dir, &["merge", "--quiet", "--no-edit", "one"]);
    git(dir, &["merge", "--quiet", "--no-edit", "--no-ff", "two"]);

    let check = String::from_utf8(project.run_ok(&["check"]).stdout).unwrap();
    assert_eq!(check, ".trurlic/ is consistent\n");
    assert_eq!(
        project.read("decisions/rotate-keys.toml"),
        decision("auth", "Rotate keys", "")
            + "depends_on = [\n    \"expire-sessions\",\n    \"sign-tokens\",\n    \"verify-audience\",\n]\n"
    );
    for name in ["expire-sessions", "verify-audience"] {
        assert!(
            project.node(&format!("decisions/{name}.toml")).is_file(),
            "{name}"
        );
    }
}
