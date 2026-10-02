//! Both servers' watchers, with a reload paused at `watcher.reload` while it
//! holds the shared lock. A write from another process waits for the reload
//! and is then picked up, and a write from the server itself completes.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::harness::{MapServer, McpClient, Project};

/// Longer than several lock polls, shorter than the 5 s lock timeout.
const HOLD: Duration = Duration::from_millis(500);

/// How long a reload may take to show up before the test fails.
const SETTLE: Duration = Duration::from_secs(10);

/// The first watcher reload, armed to pause until `marker` is deleted.
struct PausedReload {
    marker: PathBuf,
}

impl PausedReload {
    fn arm(project: &Project) -> Self {
        Self {
            marker: project.path().join("reload.paused"),
        }
    }

    fn env(&self) -> [(&str, &str); 2] {
        [
            ("TRURLIC_FAILPOINT", "watcher.reload:1"),
            ("TRURLIC_FAILPOINT_PAUSE", self.marker.to_str().unwrap()),
        ]
    }
}

fn wait_until_paused(marker: &Path) {
    let deadline = Instant::now() + SETTLE;
    while !marker.exists() {
        assert!(Instant::now() < deadline, "the watcher never reloaded");
        thread::sleep(Duration::from_millis(10));
    }
}

/// Delete the marker after [`HOLD`], from another thread, so the test thread
/// can block on a request meanwhile.
fn resume_after_hold(marker: PathBuf) -> thread::JoinHandle<()> {
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
    marker: &Path,
    served: impl FnMut() -> BTreeSet<String>,
) {
    project.run_ok(&["add", "component", "beta"]);
    wait_until_paused(marker);

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

    fs::remove_file(marker).unwrap();
    assert!(writer.wait().unwrap().success());
    assert_converges(project, served);
}

#[test]
fn mcp_watcher_serves_a_write_that_waited_for_its_reload() {
    let project = Project::init();
    project.run_ok(&["add", "component", "alpha"]);
    let paused = PausedReload::arm(&project);
    let (mut client, _) = McpClient::connect_with_env(&project, &paused.env());

    external_write_waits_for_the_reload_and_is_served(&project, &paused.marker, || {
        names(&client.call_tool("get_architecture", json!({}))["components"])
    });
}

#[test]
fn map_watcher_serves_a_write_that_waited_for_its_reload() {
    let project = Project::init();
    project.run_ok(&["add", "component", "alpha"]);
    let paused = PausedReload::arm(&project);
    let map = MapServer::start_with_env(&project, &paused.env());

    external_write_waits_for_the_reload_and_is_served(&project, &paused.marker, || {
        let (status, graph) = map.request("GET", "/api/graph", None);
        assert_eq!(status, 200, "{graph}");
        names(&graph["components"])
    });
}

#[test]
fn mcp_write_during_a_reload_completes() {
    let project = Project::init();
    let paused = PausedReload::arm(&project);
    let (mut client, _) = McpClient::connect_with_env(&project, &paused.env());
    project.run_ok(&["add", "component", "alpha"]);
    wait_until_paused(&paused.marker);

    let resume = resume_after_hold(paused.marker);
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
    let paused = PausedReload::arm(&project);
    let map = MapServer::start_with_env(&project, &paused.env());
    project.run_ok(&["add", "component", "alpha"]);
    wait_until_paused(&paused.marker);

    let resume = resume_after_hold(paused.marker);
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
