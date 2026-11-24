//! `TRURLIC_FAILPOINT` stops the binary at a named site of a commit, with
//! an abort or an injected I/O error. The next run recovers the graph as
//! it was before the commit or as the commit leaves it, never in between.
//! `trurlic fmt` is the commit: it rewrites several files in one.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::thread;
use std::time::Duration;

use serde_json::json;

use crate::harness::{McpClient, Pause, Project, decision};

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

/// The next CLI run finishes the interrupted write and finds a graph with
/// no issue, with no journal or temp file left.
fn assert_recovers(project: &Project) {
    project.run_ok(&["check"]);
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
    fn new(setup: fn(&Project), args: &'static [&'static str]) -> Self {
        let project = Project::init();
        setup(&project);
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
    /// commit's `entries` renames. A stop before the journal recovers the
    /// old store, every later one the new store.
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

/// Three files a hand edit left out of canonical form, which `fmt` rewrites
/// in one commit.
fn hand_edits(project: &Project) {
    project.write("components/auth.toml", "description='Auth'\n");
    project.write("components/store.toml", "description='Store'\n");
    project.write(
        "decisions/use-jwt.toml",
        &decision("auth", "Use JWT", "tags = [\"b\", \"a\"]\n"),
    );
}

/// Renamed: the two component files, then the decision file.
#[test]
fn every_stop_in_a_format_recovers_the_old_or_the_new_graph() {
    Scenario::new(hand_edits, &["fmt"]).assert_every_stop_recovers(3);
}

/// The second rename fails once the journal is in place: the command
/// reports the commit as not applied, and the next command applies it.
#[test]
fn a_failed_rename_is_applied_by_the_next_command() {
    let project = Project::init();
    hand_edits(&project);

    let output = project
        .command()
        .arg("fmt")
        .env(FAILPOINT, "commit.rename:2")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1), "{}", output.status);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not finished"), "{stderr}");
    assert!(journal_exists(&project));
    assert_eq!(
        project.read("components/store.toml"),
        "description='Store'\n"
    );
    assert_recovers(&project);
    assert_eq!(
        project.read("components/store.toml"),
        "description = \"Store\"\n"
    );
}

/// A read command runs while another process holds the lock with its files
/// staged. It waits for the commit, leaves its files alone, and then reads
/// the commit whole.
#[test]
fn status_during_a_staged_commit_waits_for_it() {
    let project = Project::init();
    hand_edits(&project);
    let pause = Pause::arm(&project, "commit.staged:1");
    let mut writer = project
        .command()
        .arg("fmt")
        .envs(pause.env())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    pause.wait();
    let staged = temp_files(&project);
    assert!(!staged.is_empty());

    let status = project
        .command()
        .arg("status")
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    // Several lock polls, well inside the lock timeout.
    thread::sleep(Duration::from_millis(500));
    assert_eq!(temp_files(&project), staged);
    pause.resume();

    let output = status.wait_with_output().unwrap();
    assert!(output.status.success(), "{}", output.status);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("components: 2"), "{stdout}");
    assert!(
        !stdout.contains("canonical"),
        "the whole commit is read: {stdout}"
    );
    assert!(writer.wait().unwrap().success());
    assert_recovers(&project);
}

/// A CLI commit fails after its journal is in place. The server's next
/// read finishes that commit before it loads.
#[test]
fn a_server_read_applies_a_commit_another_process_left_unfinished() {
    let project = Project::init();
    hand_edits(&project);
    let (mut client, _) = McpClient::connect(&project);
    let output = project
        .command()
        .arg("fmt")
        .env(FAILPOINT, "commit.rename:2")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{}", output.status);
    assert!(journal_exists(&project));

    let context = client.call_tool("get_context", json!({ "component": "auth" }));

    assert_eq!(context["component"]["description"], "Auth", "{context}");
    assert!(!journal_exists(&project));
    assert_eq!(
        project.read("components/store.toml"),
        "description = \"Store\"\n"
    );
    assert_recovers(&project);
}

/// A malformed spec is reported and ignored; it never aborts.
#[test]
fn malformed_failpoint_is_ignored() {
    let project = Project::init();
    hand_edits(&project);

    let output = project
        .command()
        .arg("fmt")
        .env(FAILPOINT, "commit.nowhere:1")
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", output.status);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ignoring malformed TRURLIC_FAILPOINT"),
        "{stderr}"
    );
    assert_eq!(
        project.read("components/auth.toml"),
        "description = \"Auth\"\n"
    );
}
