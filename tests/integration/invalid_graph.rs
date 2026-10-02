//! A graph that is already invalid, as a hand edit or an older release can
//! leave it: `check` reports it the same way on every run.

use std::fs::{self, OpenOptions};
use std::io::Write;

use crate::harness::Project;

/// `sign-tokens` and `rotate-keys` depend on each other, `expire-sessions`
/// depends into that cycle, and `keep-old-api` belongs to a component whose
/// file is gone.
fn invalid_project() -> Project {
    let project = Project::init();
    project.run_ok(&["add", "component", "auth"]);
    project.run_ok(&["add", "component", "legacy"]);
    for (component, choice) in [
        ("auth", "Sign tokens"),
        ("auth", "Rotate keys"),
        ("auth", "Expire sessions"),
        ("legacy", "Keep old API"),
    ] {
        let reason = "Recorded so the fixture has something to break";
        project.run_ok(&["decide", component, "--choice", choice, "--reason", reason]);
    }
    for (from, to) in [
        ("sign-tokens", "rotate-keys"),
        ("rotate-keys", "sign-tokens"),
        ("expire-sessions", "sign-tokens"),
    ] {
        add_depends_on(&project, from, to);
    }
    fs::remove_file(project.path().join(".trurlic/components/legacy.toml")).unwrap();
    project
}

fn add_depends_on(project: &Project, from: &str, to: &str) {
    let mut graph = OpenOptions::new()
        .append(true)
        .open(project.path().join(".trurlic/graph.toml"))
        .unwrap();
    write!(
        graph,
        "\n[[edges]]\nfrom = \"{from}\"\nto = \"{to}\"\nkind = \"depends_on\"\n"
    )
    .unwrap();
}

/// Each run is a new process with new hash-map seeds.
#[test]
fn check_reports_a_cycle_once_and_identically_on_every_run() {
    let project = invalid_project();
    let runs: Vec<Vec<u8>> = (0..5)
        .map(|_| {
            let output = project.command().arg("check").output().unwrap();
            assert!(!output.status.success());
            output.stderr
        })
        .collect();

    assert!(runs.windows(2).all(|pair| pair[0] == pair[1]));
    let report = String::from_utf8(runs[0].clone()).unwrap();
    let cycles: Vec<&str> = report
        .lines()
        .filter(|line| line.contains("cycle"))
        .collect();
    assert_eq!(
        cycles,
        ["  error: depends_on cycle among `rotate-keys`, `sign-tokens`"]
    );
}
