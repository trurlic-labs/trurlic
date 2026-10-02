//! Both servers' watchers, with a reload paused at `watcher.reload` while it
//! holds the shared lock. A write from another process waits for the reload
//! and is then picked up, and a write from the server itself completes. A
//! commit another process left unfinished is applied by the watcher.

use std::collections::BTreeSet;
use std::fs;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::harness::{MapServer, McpClient, Pause, Project};

/// Longer than several lock polls, shorter than the 5 s lock timeout.
const HOLD: Duration = Duration::from_millis(500);

/// How long a reload may take to show up before the test fails.
const SETTLE: Duration = Duration::from_secs(10);

/// The first watcher reload, armed to pause.
fn pause_first_reload(project: &Project) -> Pause {
    Pause::arm(project, "watcher.reload:1")
}

/// Delete the marker after [`HOLD`], from another thread, so the test thread
/// can block on a request meanwhile.
fn resume_after_hold(pause: &Pause) -> thread::JoinHandle<()> {
    let marker = pause.marker().to_path_buf();
    thread::spawn(move || {
        thread::sleep(HOLD);
        fs::remove_file(marker).unwrap();
    })
}

fn components_on_disk(project: &Project) -> BTreeSet<String> {
    fs::read_dir(project.path().join(".trurlic/components"))
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            path.file_stem().unwrap().to_str().unwrap().to_owned()
        })
        .collect()
}

fn names(components: &Value) -> BTreeSet<String> {
    components
        .as_array()
        .unwrap()
        .iter()
        .map(|component| component["name"].as_str().unwrap().to_owned())
        .collect()
}

/// Poll `served` until it equals the components on disk.
fn assert_converges(project: &Project, mut served: impl FnMut() -> BTreeSet<String>) {
    let on_disk = components_on_disk(project);
    let deadline = Instant::now() + SETTLE;
    loop {
        let current = served();
        if current == on_disk {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "served {current:?}, disk holds {on_disk:?}"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

/// While the reload holds the shared lock, a CLI write waits for it; once
/// the reload ends, the write lands and its events start another reload.
fn external_write_waits_for_the_reload_and_is_served(
    project: &Project,
    pause: &Pause,
    served: impl FnMut() -> BTreeSet<String>,
) {
    project.run_ok(&["add", "component", "beta"]);
    pause.wait();

    let mut writer = project
        .command()
        .args(["add", "component", "gamma"])
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    thread::sleep(HOLD);
    assert!(
        writer.try_wait().unwrap().is_none(),
        "the write committed during the reload"
    );
    assert!(!components_on_disk(project).contains("gamma"));

    pause.resume();
    assert!(writer.wait().unwrap().success());
    assert_converges(project, served);
}

#[test]
fn mcp_watcher_serves_a_write_that_waited_for_its_reload() {
    let project = Project::init();
    project.run_ok(&["add", "component", "alpha"]);
    let paused = pause_first_reload(&project);
    let (mut client, _) = McpClient::connect_with_env(&project, &paused.env());

    external_write_waits_for_the_reload_and_is_served(&project, &paused, || {
        names(&client.call_tool("get_architecture", json!({}))["components"])
    });
}

#[test]
fn map_watcher_serves_a_write_that_waited_for_its_reload() {
    let project = Project::init();
    project.run_ok(&["add", "component", "alpha"]);
    let paused = pause_first_reload(&project);
    let map = MapServer::start_with_env(&project, &paused.env());

    external_write_waits_for_the_reload_and_is_served(&project, &paused, || {
        let (status, graph) = map.request("GET", "/api/graph", None);
        assert_eq!(status, 200, "{graph}");
        names(&graph["components"])
    });
}

#[test]
fn mcp_write_during_a_reload_completes() {
    let project = Project::init();
    let paused = pause_first_reload(&project);
    let (mut client, _) = McpClient::connect_with_env(&project, &paused.env());
    project.run_ok(&["add", "component", "alpha"]);
    paused.wait();

    let resume = resume_after_hold(&paused);
    client.call_tool("add_component", json!({ "name": "beta" }));
    resume.join().unwrap();

    assert_eq!(
        components_on_disk(&project),
        BTreeSet::from(["alpha".to_owned(), "beta".to_owned()])
    );
}

#[test]
fn map_write_during_a_reload_completes() {
    let project = Project::init();
    let paused = pause_first_reload(&project);
    let map = MapServer::start_with_env(&project, &paused.env());
    project.run_ok(&["add", "component", "alpha"]);
    paused.wait();

    let resume = resume_after_hold(&paused);
    let (status, body) = map.request("POST", "/api/component", Some(&json!({ "name": "beta" })));
    resume.join().unwrap();

    assert_eq!(status, 200, "{body}");
    assert_eq!(
        components_on_disk(&project),
        BTreeSet::from(["alpha".to_owned(), "beta".to_owned()])
    );
}

/// `.state/` is not authoritative: with its counter deleted, the next commit
/// writes generation 1 while the server serves 2. The watcher must still
/// serve that commit.
#[test]
fn mcp_watcher_serves_writes_after_the_counter_is_deleted() {
    let project = Project::init();
    let (mut client, _) = McpClient::connect(&project);
    client.call_tool("add_component", json!({ "name": "alpha" }));
    client.call_tool("add_component", json!({ "name": "beta" }));

    fs::remove_file(project.path().join(".trurlic/.state/generation")).unwrap();
    project.run_ok(&["add", "component", "gamma"]);

    assert_converges(&project, || {
        names(&client.call_tool("get_architecture", json!({}))["components"])
    });
}

/// A CLI commit fails after its journal is in place. The server's watcher
/// sees its renames, finds the journal, applies the commit and serves it,
/// with no write of its own.
#[test]
fn mcp_watcher_applies_a_commit_another_process_left_unfinished() {
    let project = Project::init();
    let (mut client, _) = McpClient::connect(&project);

    let output = project
        .command()
        .args(["add", "component", "auth"])
        .env("TRURLIC_FAILPOINT", "commit.rename:2")
        .output()
        .unwrap();
    assert!(!output.status.success());

    let journal = project.path().join(".trurlic/.state/txn.toml");
    let deadline = Instant::now() + SETTLE;
    while journal.exists() {
        assert!(Instant::now() < deadline, "the journal was never applied");
        thread::sleep(Duration::from_millis(50));
    }
    assert_converges(&project, || {
        names(&client.call_tool("get_architecture", json!({}))["components"])
    });
    assert!(components_on_disk(&project).contains("auth"));
}
