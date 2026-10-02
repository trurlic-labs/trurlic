//! `TRURLIC_FAILPOINT` stops the binary at a named site of a commit, with
//! an abort or an injected I/O error. The next run recovers the graph as
//! it was before the commit or as the commit leaves it, never in between.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::process::{ExitStatus, Stdio};

use serde_json::json;

use crate::harness::{McpClient, Pause, Project};

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

/// The next CLI run finishes the interrupted write and finds a consistent
/// graph, with no journal or temp file left.
fn assert_recovers(project: &Project) {
    let output = project.run_ok(&["check"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("is consistent"), "{stdout}");
    assert_eq!(temp_files(project), BTreeSet::new(), "temp files survived");
    assert!(!journal_exists(project), "the journal survived");
}

fn temp_files(project: &Project) -> BTreeSet<String> {
    fs::read_dir(project.path().join(".trurlic/.state/tmp"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect()
}

fn journal_exists(project: &Project) -> bool {
    project.path().join(".trurlic/.state/txn.toml").exists()
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

/// Every file of a store outside `.state/`, by path below `.trurlic/`.
type Snapshot = BTreeMap<String, String>;

fn snapshot(project: &Project) -> Snapshot {
    fn walk(root: &Path, dir: &Path, files: &mut Snapshot) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                if !path.ends_with(".state") {
                    walk(root, &path, files);
                }
            } else {
                let key = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_owned();
                files.insert(key, fs::read_to_string(&path).unwrap());
            }
        }
    }
    let root = project.path().join(".trurlic");
    let mut files = Snapshot::new();
    walk(&root, &root, &mut files);
    files
}

/// Replace the store's files outside `.state/` with `files`.
fn restore(project: &Project, files: &Snapshot) {
    let root = project.path().join(".trurlic");
    for path in snapshot(project).keys() {
        fs::remove_file(root.join(path)).unwrap();
    }
    for (path, content) in files {
        fs::write(root.join(path), content).unwrap();
    }
}

/// One commit, run on copies of one store.
struct Scenario {
    args: &'static [&'static str],
    /// The store the commit starts from.
    before: Snapshot,
    /// The store the commit leaves when nothing stops it.
    after: Snapshot,
}

impl Scenario {
    fn new(setup: &[&[&str]], args: &'static [&'static str]) -> Self {
        let project = Project::init();
        for command in setup {
            project.run_ok(command);
        }
        let before = snapshot(&project);
        project.run_ok(args);
        Self {
            args,
            before,
            after: snapshot(&project),
        }
    }

    fn fresh_copy(&self) -> Project {
        let project = Project::init();
        restore(&project, &self.before);
        project
    }

    /// Stop the commit at `spec` on a fresh copy, recover, and return what
    /// the store holds then.
    fn recovered_from(&self, spec: &str) -> Snapshot {
        let project = self.fresh_copy();
        run_aborting(&project, self.args, spec);
        assert_recovers(&project);
        snapshot(&project)
    }

    /// Abort before the journal, at the journal, and after each of the
    /// commit's `entries` renames and removals. A stop before the journal
    /// recovers the old store, every later one the new store.
    fn assert_every_stop_recovers(&self, entries: usize) {
        assert_ne!(self.before, self.after);
        assert_eq!(self.recovered_from("commit.staged:1"), self.before);
        assert_eq!(self.recovered_from("commit.journaled:1"), self.after);
        for n in 1..=entries {
            let spec = format!("commit.applied:{n}");
            assert_eq!(self.recovered_from(&spec), self.after, "stopped at {spec}");
        }

        // The commit has no further entry: armed past it, the run completes.
        let project = self.fresh_copy();
        let past = format!("commit.applied:{}", entries + 1);
        let output = project
            .command()
            .args(self.args)
            .env(FAILPOINT, &past)
            .output()
            .unwrap();
        assert!(output.status.success(), "{past}: {}", output.status);
    }
}

const ADD_DECISION: &[&[&str]] = &[
    &["add", "component", "auth"],
    &[
        "decide",
        "auth",
        "--choice",
        "Use JWT",
        "--reason",
        "Stateless verification",
    ],
];

/// Renamed: the new component file, its decision, then `graph.toml`;
/// removed: the old component file.
#[test]
fn every_stop_in_a_rename_recovers_the_old_or_the_new_graph() {
    Scenario::new(ADD_DECISION, &["rename", "component", "auth", "identity"])
        .assert_every_stop_recovers(4);
}

/// Renamed: `graph.toml`; removed: the decision file. A stop between the
/// two used to leave the file behind, so `status` counted the decision.
#[test]
fn every_stop_in_a_decision_removal_recovers_the_old_or_the_new_graph() {
    Scenario::new(ADD_DECISION, &["remove", "decision", "use-jwt"]).assert_every_stop_recovers(2);
}

/// The second rename fails once the journal is in place: the command
/// reports the commit as not applied, and the next command applies it.
#[test]
fn a_failed_rename_is_applied_by_the_next_command() {
    let project = Project::init();

    let output = project
        .command()
        .args(["add", "component", "auth"])
        .env(FAILPOINT, "commit.rename:2")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1), "{}", output.status);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not applied"), "{stderr}");
    assert!(component_file_exists(&project, "auth"));
    assert!(!graph_lists(&project, "auth"));
    assert!(journal_exists(&project));
    assert_recovers(&project);
    assert!(graph_lists(&project, "auth"));
}

/// The server's own commit fails at its first rename; its next write
/// applies that commit before its own.
#[test]
fn a_server_write_applies_its_failed_commit_first() {
    let project = Project::init();
    let (mut client, _) = McpClient::connect_with_env(&project, &[(FAILPOINT, "commit.rename:1")]);

    let envelope = client.request(
        "tools/call",
        json!({ "name": "add_component", "arguments": { "name": "auth" } }),
    );
    assert_eq!(envelope["isError"], true, "{envelope}");
    let text = envelope["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("not applied"), "{text}");

    client.call_tool("add_component", json!({ "name": "storage" }));

    assert!(graph_lists(&project, "auth"));
    assert!(graph_lists(&project, "storage"));
    assert_recovers(&project);
}

/// A read command runs while another process holds the lock with its files
/// staged. It leaves them alone, and the commit completes.
#[test]
fn status_during_a_staged_commit_leaves_it_to_complete() {
    let project = Project::init();
    let pause = Pause::arm(&project, "commit.staged:1");
    let mut writer = project
        .command()
        .args(["add", "component", "auth"])
        .envs(pause.env())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    pause.wait();
    let staged = temp_files(&project);
    assert!(!staged.is_empty());

    let output = project.run_ok(&["status"]);

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("components: 0"), "{stdout}");
    assert_eq!(temp_files(&project), staged);
    pause.resume();
    assert!(writer.wait().unwrap().success());
    assert!(graph_lists(&project, "auth"));
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
