//! The history of a shared graph in its own git repository, over the built
//! binary: `init` makes the repository, each write is one commit after one
//! of any hand edit, a stopped write is committed by the next opener once,
//! and no caller's environment or configuration breaks or redirects a
//! commit. Under `tracked` no commit is made. `check` and `status` report
//! what the history lacks without writing to it.

use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use crate::harness::{Project, decision, git, graph_git, graph_log, record};

/// The decision file a hand edit writes out of canonical form, its tags
/// out of order: `trurlic fmt` rewrites it.
const EDITED: &str = "decisions/sign-tokens.toml";

fn hand_edit(project: &Project) -> String {
    let text = decision("project", "Sign tokens", "tags = [\"b\", \"a\"]\n");
    project.write(EDITED, &text);
    text
}

/// The bytes the commit `rev` of the graph's history holds for `file`.
fn committed(project: &Project, rev: &str, file: &str) -> String {
    graph_git(
        &project.graph(),
        &["cat-file", "blob", &format!("{rev}:{file}")],
    )
}

#[test]
fn init_makes_the_graph_a_repository_whose_first_commit_is_init() {
    let project = Project::shared();

    assert_eq!(graph_log(&project.graph()), ["init"]);
    assert_eq!(
        committed(&project, "HEAD", "project.toml"),
        project.read("project.toml")
    );
    // `.state/` holds the lock `init` took, and the repository ignores it.
    assert!(project.node(".state/lock").exists());
    assert_eq!(
        graph_git(
            &project.graph(),
            &["status", "--porcelain", "--untracked-files=all"]
        ),
        ""
    );
    assert_eq!(project.run_ok(&["check"]).status.code(), Some(0));
}

/// A hand edit followed by a write: the hand edit is its own commit, with
/// the bytes it left on disk, and the write's holds the write alone. A
/// write with nothing to do makes no commit.
#[test]
fn a_hand_edit_followed_by_a_write_is_two_commits() {
    let project = Project::shared();
    let edited = hand_edit(&project);

    project.run_ok(&["fmt"]);

    assert_eq!(graph_log(&project.graph()), ["fmt", "hand edit", "init"]);
    assert_eq!(committed(&project, "HEAD~1", EDITED), edited);
    assert_eq!(committed(&project, "HEAD", EDITED), project.read(EDITED));
    assert_ne!(project.read(EDITED), edited);
    project.run_ok(&["fmt"]);
    assert_eq!(graph_log(&project.graph()).len(), 3);
}

/// A canonical hand edit leaves `fmt` nothing to rewrite, and it commits
/// the hand edit, so `check` finds the history settled.
#[test]
fn fmt_commits_a_hand_edit_it_has_nothing_to_rewrite_for() {
    let project = Project::shared();
    project.write("components/auth.toml", "description = \"Auth\"\n");
    let stderr = String::from_utf8(project.run_ok(&["check"]).stderr).unwrap();
    assert!(
        stderr.contains(
            "1 file(s) differ from the last commit of the graph's history: \
             components/auth.toml; `trurlic fmt`, or the next write, commits them as \
             `hand edit`"
        ),
        "{stderr}"
    );

    project.run_ok(&["fmt"]);

    assert_eq!(graph_log(&project.graph()), ["hand edit", "init"]);
    let stdout = String::from_utf8(project.run_ok(&["check"]).stdout).unwrap();
    assert_eq!(stdout, ".trurlic/ is consistent\n");
}

/// Under `tracked` the project's git holds the history: a write commits
/// nothing and makes no repository, and neither `check` nor `status` asks
/// about one.
#[test]
fn under_tracked_no_commit_is_made() {
    let project = Project::init();
    let before = graph_git(project.path(), &["rev-parse", "HEAD"]);
    hand_edit(&project);

    project.run_ok(&["fmt"]);

    assert!(!project.graph().join(".git").exists());
    assert_eq!(graph_git(project.path(), &["rev-parse", "HEAD"]), before);
    let status = String::from_utf8(project.run_ok(&["status"]).stdout).unwrap();
    assert!(!status.contains("history"), "{status}");
}

/// A write started from a git hook of another repository inherits that
/// repository's variables; the commit goes to the graph's repository
/// alone, and the other one's history and index are as they were.
#[test]
fn a_write_with_git_dir_set_to_another_repository_commits_only_to_the_graph() {
    let project = Project::shared();
    hand_edit(&project);
    let other = TempDir::new().unwrap();
    git(other.path(), &["init", "--quiet", "--initial-branch=main"]);
    fs::write(other.path().join("file"), "").unwrap();
    git(other.path(), &["add", "file"]);
    git(other.path(), &["commit", "--quiet", "-m", "other"]);
    let other_git = other.path().join(".git");
    let index = fs::read(other_git.join("index")).unwrap();

    let output = project
        .command()
        .arg("fmt")
        .env("GIT_DIR", &other_git)
        .env("GIT_WORK_TREE", other.path())
        .env("GIT_INDEX_FILE", other_git.join("index"))
        .env("GIT_COMMON_DIR", &other_git)
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    assert_eq!(graph_log(&project.graph()), ["fmt", "hand edit", "init"]);
    assert_eq!(graph_log(other.path()), ["other"]);
    assert_eq!(fs::read(other_git.join("index")).unwrap(), index);
}

#[test]
fn a_write_while_git_holds_the_index_is_refused_and_writes_nothing() {
    let project = Project::shared();
    let edited = hand_edit(&project);
    let lock = project.graph().join(".git/index.lock");
    fs::write(&lock, "").unwrap();

    let stderr = project.run_refused(&["fmt"]);

    assert!(
        stderr.contains(&format!(
            "error: {}: git holds the index of the graph's repository",
            lock.display()
        )),
        "{stderr}"
    );
    assert!(stderr.contains("the write is refused"), "{stderr}");
    assert_eq!(project.read(EDITED), edited);
    assert!(!project.node(".state/txn.toml").exists());
    assert_eq!(graph_log(&project.graph()), ["init"]);
}

/// `check` fails on a leftover index lock and warns about files the
/// history does not hold; `status` names both. Neither writes to the
/// repository: the index keeps its bytes.
#[test]
fn check_and_status_report_an_uncommitted_tree_and_a_leftover_index_lock() {
    let project = Project::shared();
    hand_edit(&project);
    let lock = project.graph().join(".git/index.lock");
    fs::write(&lock, "").unwrap();
    let index = project.graph().join(".git/index");
    let before = fs::read(&index).unwrap();

    let stderr = project.run_refused(&["check"]);
    let status = String::from_utf8(project.run_ok(&["status"]).stdout).unwrap();

    let uncommitted = format!(
        "{}: 1 file(s) differ from the last commit of the graph's history: {EDITED};",
        project.graph().display()
    );
    let locked = format!(
        "{}: git holds the index of the graph's repository, or a git process that stopped \
         left this lock; every write is refused until it is gone",
        lock.display()
    );
    assert!(stderr.contains(&format!("  error: {locked}")), "{stderr}");
    assert!(
        stderr.contains(&format!("  warning: {uncommitted}")),
        "{stderr}"
    );
    assert!(
        stderr.contains("error: 1 consistency error(s) found"),
        "{stderr}"
    );
    assert!(status.contains(&format!("history: {locked}")), "{status}");
    assert!(
        status.contains(&format!("history: {uncommitted}")),
        "{status}"
    );
    assert_eq!(fs::read(&index).unwrap(), before);
}

/// With no `git` on `PATH`, `init` of a shared graph is refused and leaves
/// no `.trurlic/` behind, and a write to a shared graph is refused before
/// anything is written.
#[test]
fn a_missing_git_is_refused_at_init_and_at_a_write() {
    let empty = TempDir::new().unwrap();
    let project = Project::without_store();
    let no_git = |args: &[&str]| {
        project
            .command()
            .args(args)
            .env("PATH", empty.path())
            .output()
            .unwrap()
    };

    let output = no_git(&["init", "--placement", "shared"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("error: cannot run `git`"), "{stderr}");
    assert!(!project.graph().exists());

    project.run_ok(&["init", "--placement", "shared"]);
    let edited = hand_edit(&project);
    let output = no_git(&["fmt"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(project.read(EDITED), edited);
    assert!(!project.node(".state/txn.toml").exists());
}

/// A user configuration, an environment and a repository configuration
/// each set to break or redirect a commit: hooks that fail and leave a
/// mark, a commit template and a cleanup that strips lines, signing with a
/// program that fails, an identity, dates, a template directory and a
/// default branch, ignore rules for every `.toml`, line-end conversion and
/// a filter that fails, a file monitor, and a work tree elsewhere. Every
/// commit is the fixed identity with git's dates, on `main`, holding the
/// bytes on disk, and nothing ran.
#[cfg(unix)]
#[test]
fn no_configuration_or_environment_breaks_or_redirects_a_commit() {
    let hostile = Hostile::new();
    let project = Project::without_store();
    let run = |args: &[&str]| {
        let output = hostile
            .apply(&mut project.command())
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "trurlic {args:?}: {output:?}");
    };
    run(&["init", "--placement", "shared"]);
    let config = project.graph().join(".git/config");
    let local = fs::read_to_string(&config).unwrap();
    fs::write(&config, format!("{local}{}", hostile.settings())).unwrap();
    project.write(".gitattributes", "* text eol=crlf ident filter=broken\n");
    let edited = decision("project", "A plan $Id$", "").replace('\n', "\r\n");
    project.write(EDITED, &edited);

    run(&["fmt"]);

    let graph = project.graph();
    assert_eq!(graph_log(&graph), ["fmt", "hand edit", "init"]);
    let identities = graph_git(&graph, &["log", "--format=%an <%ae> %cn <%ce> %at %ct"]);
    for line in identities.lines() {
        let fields: Vec<&str> = line.split(' ').collect();
        assert_eq!(
            fields[..4],
            [
                "trurlic",
                "<trurlic@trurlic.invalid>",
                "trurlic",
                "<trurlic@trurlic.invalid>"
            ],
            "{line}"
        );
        for stamp in &fields[4..] {
            assert!(stamp.parse::<u64>().unwrap() > 1_000_000_000, "{line}");
        }
    }
    assert_eq!(
        graph_git(&graph, &["symbolic-ref", "HEAD"]),
        "refs/heads/main\n"
    );
    assert_eq!(committed(&project, "HEAD~1", EDITED), edited);
    assert_eq!(committed(&project, "HEAD", EDITED), project.read(EDITED));
    assert_eq!(
        committed(&project, "HEAD", "project.toml"),
        project.read("project.toml")
    );
    assert!(!graph.join(".git/hooks").exists());
    assert_eq!(
        fs::read_to_string(graph.join(".git/info/exclude")).unwrap(),
        "/.state/\n"
    );
    assert!(
        !hostile.mark.exists(),
        "{}",
        fs::read_to_string(&hostile.mark).unwrap()
    );
}

/// A user's configuration and environment, and the lines a repository's
/// configuration takes, set against a commit. Every program they name
/// appends to `mark` and fails.
#[cfg(unix)]
struct Hostile {
    dir: TempDir,
    mark: PathBuf,
}

#[cfg(unix)]
impl Hostile {
    fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let root = dir.path();
        let mark = root.join("mark");
        let script = |path: &Path| {
            fs::write(
                path,
                format!("#!/bin/sh\necho \"$0\" >> '{}'\nexit 1\n", mark.display()),
            )
            .unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        };
        for hooks in ["hooks", "template/hooks"] {
            fs::create_dir_all(root.join(hooks)).unwrap();
            for hook in [
                "pre-commit",
                "prepare-commit-msg",
                "commit-msg",
                "post-commit",
                "post-index-change",
                "reference-transaction",
            ] {
                script(&root.join(hooks).join(hook));
            }
        }
        script(&root.join("program"));
        fs::create_dir_all(root.join("template/info")).unwrap();
        fs::write(root.join("template/info/exclude"), "*\n").unwrap();
        fs::create_dir_all(root.join("xdg/git")).unwrap();
        for ignore in ["ignore", "xdg/git/ignore"] {
            fs::write(root.join(ignore), "*.toml\n").unwrap();
        }
        for attributes in ["attributes", "xdg/git/attributes"] {
            fs::write(
                root.join(attributes),
                "* text eol=crlf ident filter=broken\n",
            )
            .unwrap();
        }
        fs::write(root.join("message"), "Written by someone else\n").unwrap();
        let config = root.join("gitconfig");
        let hostile = Self { dir, mark };
        // A level zlib refuses: every object git would write fails, unless
        // the user's configuration is not read at all.
        let user = format!("{}[core]\n\tcompression = 99\n", hostile.settings());
        fs::write(config, user).unwrap();
        hostile
    }

    /// Configuration that breaks or redirects a commit, as a user's file or
    /// as lines of a repository's own.
    fn settings(&self) -> String {
        let root = self.dir.path().display();
        format!(
            "[user]\n\tname = Mallory\n\temail = mallory@example.com\n\tuseConfigOnly = true\n\
             [author]\n\tname = Mallory\n[committer]\n\tname = Mallory\n\
             [core]\n\thooksPath = {root}/hooks\n\texcludesFile = {root}/ignore\n\
             \tattributesFile = {root}/attributes\n\tautocrlf = true\n\
             \tfsmonitor = {root}/program\n\teditor = {root}/program\n\tcommentChar = O\n\
             \tworktree = {root}\n\
             [commit]\n\ttemplate = {root}/message\n\tgpgSign = true\n\tcleanup = strip\n\
             \tverbose = true\n\
             [gpg]\n\tprogram = {root}/program\n\
             [init]\n\ttemplateDir = {root}/template\n\tdefaultBranch = elsewhere\n\
             [filter \"broken\"]\n\tclean = {root}/program\n\tsmudge = {root}/program\n\
             \trequired = true\n"
        )
    }

    /// `command` with this user's configuration and environment.
    fn apply<'c>(&self, command: &'c mut std::process::Command) -> &'c mut std::process::Command {
        let root = self.dir.path();
        command
            .env("GIT_CONFIG_GLOBAL", root.join("gitconfig"))
            .env("XDG_CONFIG_HOME", root.join("xdg"))
            .env("GIT_TEMPLATE_DIR", root.join("template"))
            .env("GIT_AUTHOR_NAME", "Mallory")
            .env("GIT_AUTHOR_EMAIL", "mallory@example.com")
            .env("GIT_COMMITTER_NAME", "Mallory")
            .env("GIT_COMMITTER_EMAIL", "mallory@example.com")
            .env("GIT_AUTHOR_DATE", "2000-01-01T00:00:00Z")
            .env("GIT_COMMITTER_DATE", "2000-01-01T00:00:00Z")
            .env("GIT_EDITOR", root.join("program"))
            .env("GIT_INDEX_FILE", root.join("index"))
    }
}

/// The stop of a write at each step after its commit point: its files in
/// place, its history index staged, its history commit made, its journal
/// deleted. The next opener
/// finishes it, and the history holds its commit once.
#[cfg(feature = "failpoints")]
#[test]
fn a_write_stopped_after_its_commit_point_is_committed_once_by_the_next_opener() {
    use crate::harness::trurlic_ok;

    for spec in [
        "commit.placed:1",
        "history.staged:2",
        "history.committed:1",
        "commit.finished:1",
    ] {
        let project = Project::shared();
        hand_edit(&project);
        let output = project
            .command()
            .arg("fmt")
            .env("TRURLIC_FAILPOINT", spec)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{spec}: {output:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(&format!("failpoint {spec} hit")),
            "{stderr}"
        );

        assert_eq!(
            trurlic_ok(project.path(), &["check"]),
            ".trurlic/ is consistent\n",
            "{spec}"
        );
        assert!(!project.node(".state/txn.toml").exists(), "{spec}");
        assert_eq!(
            graph_log(&project.graph()),
            ["fmt", "hand edit", "init"],
            "{spec}"
        );
    }
}

/// A write whose history commit fails after its files are in place: the
/// command reports the write as not finished, a read sees the files and
/// warns, a writer is refused while the commit cannot be made, and the
/// first writer after it can makes it.
#[cfg(feature = "failpoints")]
#[test]
fn a_history_commit_that_fails_is_made_by_the_next_writer() {
    use crate::harness::Pause;

    let project = Project::shared();
    hand_edit(&project);
    let pause = Pause::arm(&project, "commit.placed:1");
    let writer = project
        .command()
        .arg("fmt")
        .envs(pause.env())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    pause.wait();
    let lock = project.graph().join(".git/index.lock");
    fs::write(&lock, "").unwrap();
    pause.resume();
    let writer = writer.wait_with_output().unwrap();
    assert_eq!(writer.status.code(), Some(1), "{writer:?}");
    let stderr = String::from_utf8(writer.stderr).unwrap();
    assert!(stderr.contains("but not finished"), "{stderr}");

    let read = project.run_ok(&["status"]);
    let warning = String::from_utf8(read.stderr).unwrap();
    assert!(
        warning.contains("warning: the write is recorded in"),
        "{warning}"
    );
    assert!(project.node(".state/txn.toml").exists());
    assert!(project.run_refused(&["fmt"]).contains("but not finished"));
    fs::remove_file(&lock).unwrap();
    project.run_ok(&["fmt"]);

    assert!(!project.node(".state/txn.toml").exists());
    assert_eq!(graph_log(&project.graph()), ["fmt", "hand edit", "init"]);
}

/// A write stopped with its hand edits staged and not committed, before
/// its commit point: the graph is as it was, and the next write makes the
/// `hand edit` commit and its own, nothing more.
#[cfg(feature = "failpoints")]
#[test]
fn a_write_stopped_with_its_hand_edits_staged_leaves_them_to_the_next_write() {
    let project = Project::shared();
    let text = hand_edit(&project);
    let output = project
        .command()
        .arg("fmt")
        .env("TRURLIC_FAILPOINT", "history.staged:1")
        .output()
        .unwrap();
    assert!(!output.status.success(), "{output:?}");
    assert_eq!(project.read(EDITED), text);
    assert_eq!(graph_log(&project.graph()), ["init"]);

    project.run_ok(&["fmt"]);

    assert_eq!(graph_log(&project.graph()), ["fmt", "hand edit", "init"]);
    assert_eq!(committed(&project, "HEAD~1", EDITED), text);
    assert_eq!(committed(&project, "HEAD", EDITED), project.read(EDITED));
}

/// A write with removals whose history commit fails once its files are in
/// place, its journal narrowed to that commit and the reader stopped
/// there: the owner re-creates the removed file and edits a written one.
/// No later read removes the file again, and the commit the next opener
/// makes holds the write's own bytes and removal; both edits are left for
/// the next `hand edit` commit.
#[cfg(feature = "failpoints")]
#[test]
fn a_stopped_write_is_committed_as_it_was_and_removes_no_file_twice() {
    let project = Project::shared();
    assert!(project.apply(&batch("row 1", "auth"), &[]).status.success());
    let removed = "decisions/auth-two.toml";
    let kept = project.read(removed);
    let removals = serde_json::json!({ "origin": "row 2", "ops": [
        { "op": "rename", "name": "auth-one", "to": "auth-first" },
        { "op": "remove", "name": "auth-two" },
    ]});
    let file = project.path().join("removals.json");
    fs::write(&file, removals.to_string()).unwrap();
    let stopped = |args: &[&str], spec: &str| {
        let output = project
            .command()
            .args(args)
            .env("TRURLIC_FAILPOINT", spec)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{spec}: {output:?}");
    };
    stopped(&["apply", file.to_str().unwrap()], "commit.placed:1");
    let lock = project.graph().join(".git/index.lock");
    fs::write(&lock, "").unwrap();
    stopped(&["status"], "commit.narrowed:1");
    let journal: toml::Table = project.read(".state/txn.toml").parse().unwrap();
    assert_eq!(journal.keys().collect::<Vec<_>>(), ["history"], "{journal}");

    let written = project.read("decisions/auth-first.toml");
    project.write(removed, &kept);
    let edited = written.replace("A rule of auth", "A rule of auth, edited");
    project.write("decisions/auth-first.toml", &edited);
    project.run_ok(&["status"]);
    project.run_ok(&["status"]);
    assert_eq!(project.read(removed), kept);
    fs::remove_file(&lock).unwrap();
    project.run_ok(&["status"]);

    assert!(!project.node(".state/txn.toml").exists());
    let log = graph_log(&project.graph());
    assert_eq!(
        log[..2],
        ["apply\n\nOrigin: row 2", "apply\n\nOrigin: row 1"]
    );
    assert_eq!(
        committed(&project, "HEAD", "decisions/auth-first.toml"),
        written
    );
    let tree = graph_git(&project.graph(), &["ls-tree", "--name-only", "-r", "HEAD"]);
    assert!(!tree.contains(removed), "{tree}");
    assert_eq!(project.read(removed), kept);
    project.run_ok(&["fmt"]);
    assert_eq!(graph_log(&project.graph())[0], "hand edit");
    let changed = graph_git(
        &project.graph(),
        &["show", "--name-only", "--format=", "HEAD"],
    );
    assert_eq!(
        changed,
        "decisions/auth-first.toml\ndecisions/auth-two.toml\n"
    );
}

/// A batch that records two decisions of a new component.
fn batch(origin: &str, component: &str) -> serde_json::Value {
    let record = |name: String| record(&name, component, &format!("A rule of {component}"));
    serde_json::json!({ "origin": origin, "ops": [
        { "op": "add_component", "name": component, "description": "" },
        record(format!("{component}-one")),
        record(format!("{component}-two")),
    ]})
}

/// Over MCP, each batch is one commit of the shared graph's history,
/// named `apply`, with the batch's origin as its `Origin:` trailer and
/// no `Session:`, which no request carries; under `tracked` the project's
/// history gains no commit.
#[test]
fn each_apply_is_one_commit_with_its_origin() {
    use crate::harness::McpClient;

    let project = Project::shared();
    let (mut client, _) = McpClient::connect(&project);
    client.call_tool("apply", batch("task 18", "auth"));
    client.call_tool("apply", batch("task 19", "billing"));

    assert_eq!(
        graph_log(&project.graph()),
        [
            "apply\n\nOrigin: task 19",
            "apply\n\nOrigin: task 18",
            "init"
        ]
    );
    let changed = graph_git(
        &project.graph(),
        &["show", "--name-only", "--format=", "HEAD"],
    );
    assert_eq!(
        changed,
        "components/billing.toml\ndecisions/billing-one.toml\ndecisions/billing-two.toml\n"
    );

    let tracked = Project::init();
    let before = graph_git(tracked.path(), &["rev-parse", "HEAD"]);
    let (mut client, _) = McpClient::connect(&tracked);
    client.call_tool("apply", batch("task 18", "auth"));
    assert_eq!(graph_git(tracked.path(), &["rev-parse", "HEAD"]), before);
    assert!(!tracked.graph().join(".git").exists());
}

/// A batch with removals stopped at each step of its commit under
/// `shared`: before its journal the next opener finds the graph as it
/// was and no commit; from the journal on, the batch's files and one
/// `apply` commit with its origin.
#[cfg(feature = "failpoints")]
#[test]
fn a_stopped_batch_is_finished_by_the_next_opener_with_one_commit() {
    use crate::harness::trurlic_ok;

    let removals = serde_json::json!({ "origin": "row 2", "ops": [
        { "op": "rename", "name": "auth-one", "to": "auth-first" },
        { "op": "remove", "name": "auth-two" },
    ]});
    for (spec, lands) in [
        ("commit.staged:1", false),
        ("history.blobs:1", false),
        ("commit.journaled:1", true),
        ("commit.applied:2", true),
        ("commit.placed:1", true),
        ("history.staged:1", true),
        ("history.committed:1", true),
        ("commit.finished:1", true),
    ] {
        let project = Project::shared();
        assert!(project.apply(&batch("row 1", "auth"), &[]).status.success());
        let file = project.path().join("removals.json");
        fs::write(&file, removals.to_string()).unwrap();
        let output = project
            .command()
            .arg("apply")
            .arg(&file)
            .env("TRURLIC_FAILPOINT", spec)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{spec}: {output:?}");

        assert_eq!(
            trurlic_ok(project.path(), &["check"]),
            ".trurlic/ is consistent\n"
        );
        let log = graph_log(&project.graph());
        let names: Vec<String> = project.graph_files().into_keys().collect();
        if lands {
            assert_eq!(log[0], "apply\n\nOrigin: row 2", "{spec}");
            assert_eq!(log[1], "apply\n\nOrigin: row 1", "{spec}");
            assert_eq!(log.len(), 3, "{spec}: {log:?}");
            assert!(
                names.contains(&"decisions/auth-first.toml".to_owned()),
                "{spec}"
            );
            assert!(
                !names.contains(&"decisions/auth-two.toml".to_owned()),
                "{spec}"
            );
        } else {
            assert_eq!(log.len(), 2, "{spec}: {log:?}");
            assert!(
                names.contains(&"decisions/auth-two.toml".to_owned()),
                "{spec}"
            );
        }
    }
}

/// The hardware flushes the git processes of one command made, and their
/// writeout-only flushes, read from git's trace2 counters.
fn fsyncs(project: &Project, args: &[&str]) -> (u64, u64) {
    let trace = project.path().join("trace2.json");
    let _ = fs::remove_file(&trace);
    let output = project
        .command()
        .args(args)
        .env("GIT_TRACE2_EVENT", &trace)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let (mut flushes, mut writeouts) = (0, 0);
    for line in fs::read_to_string(&trace).unwrap().lines() {
        let event: serde_json::Value = serde_json::from_str(line).unwrap();
        if event["event"] == "counter" && event["category"] == "fsync" {
            let count = event["count"].as_u64().unwrap();
            match event["name"].as_str().unwrap() {
                "hardware-flush" => flushes += count,
                "writeout-only" => writeouts += count,
                other => panic!("an fsync counter git did not name before: {other}"),
            }
        }
    }
    (flushes, writeouts)
}

/// Thirty new decision files, and then the first commit of a repository
/// made again, which holds every graph file: each commit's objects reach
/// the disk through one hardware flush per object transaction, not one
/// per object, so the flushes do not grow with the files.
#[test]
fn a_commit_flushes_its_objects_in_one_batch_however_many_files_it_holds() {
    let project = Project::shared();
    let ops: Vec<serde_json::Value> = (0..30)
        .map(|i| record(&format!("rule-{i}"), "project", &format!("Rule {i}")))
        .collect();
    let file = project.path().join("batch.json");
    fs::write(&file, serde_json::json!({ "ops": ops }).to_string()).unwrap();

    let (flushes, writeouts) = fsyncs(&project, &["apply", file.to_str().unwrap()]);
    assert!(writeouts >= 30, "{writeouts} writeouts");
    assert!(flushes < 10, "{flushes} hardware flushes for 30 objects");

    fs::remove_dir_all(project.graph().join(".git")).unwrap();
    hand_edit(&project);
    let (flushes, writeouts) = fsyncs(&project, &["fmt"]);
    assert!(writeouts >= 32, "{writeouts} writeouts");
    assert!(
        flushes < 10,
        "{flushes} hardware flushes for the first commit"
    );
    assert_eq!(graph_log(&project.graph()), ["fmt"]);
    assert_eq!(
        graph_git(&project.graph(), &["ls-files"]).lines().count(),
        32
    );
}
