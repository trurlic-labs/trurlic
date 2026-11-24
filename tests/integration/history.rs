//! The history of a shared graph in its own git repository, over the built
//! binary: `init` makes the repository, each write is one commit after one
//! of any hand edit, a stopped write is committed by the next opener once,
//! and no caller's environment or configuration breaks or redirects a
//! commit. Under `tracked` no commit is made. `check` and `status` report
//! what the history lacks without writing to it.

use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use crate::harness::{Project, decision, git, graph_git, graph_log};

/// The decision file a hand edit writes out of canonical form, its tags
/// out of order: `trurlic fmt` rewrites it.
const EDITED: &str = "decisions/sign-tokens.toml";

/// A directory in no git work tree holding a shared graph `init` made.
fn shared() -> Project {
    let project = Project::without_store();
    project.run_ok(&["init", "--placement", "shared"]);
    project
}

fn graph(project: &Project) -> PathBuf {
    project.path().join(".trurlic")
}

fn hand_edit(project: &Project) -> String {
    let text = decision("project", "Sign tokens", "tags = [\"b\", \"a\"]\n");
    project.write(EDITED, &text);
    text
}

/// The bytes the commit `rev` of the graph's history holds for `file`.
fn committed(project: &Project, rev: &str, file: &str) -> String {
    graph_git(
        &graph(project),
        &["cat-file", "blob", &format!("{rev}:{file}")],
    )
}

#[test]
fn init_makes_the_graph_a_repository_whose_first_commit_is_init() {
    let project = shared();

    assert_eq!(graph_log(&graph(&project)), ["init"]);
    assert_eq!(
        committed(&project, "HEAD", "project.toml"),
        project.read("project.toml")
    );
    // `.state/` holds the lock `init` took, and the repository ignores it.
    assert!(project.node(".state/lock").exists());
    assert_eq!(
        graph_git(
            &graph(&project),
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
    let project = shared();
    let edited = hand_edit(&project);

    project.run_ok(&["fmt"]);

    assert_eq!(graph_log(&graph(&project)), ["fmt", "hand edit", "init"]);
    assert_eq!(committed(&project, "HEAD~1", EDITED), edited);
    assert_eq!(committed(&project, "HEAD", EDITED), project.read(EDITED));
    assert_ne!(project.read(EDITED), edited);
    project.run_ok(&["fmt"]);
    assert_eq!(graph_log(&graph(&project)).len(), 3);
}

/// A canonical hand edit leaves `fmt` nothing to rewrite, and it commits
/// the hand edit, so `check` finds the history settled.
#[test]
fn fmt_commits_a_hand_edit_it_has_nothing_to_rewrite_for() {
    let project = shared();
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

    assert_eq!(graph_log(&graph(&project)), ["hand edit", "init"]);
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

    assert!(!graph(&project).join(".git").exists());
    assert_eq!(graph_git(project.path(), &["rev-parse", "HEAD"]), before);
    let status = String::from_utf8(project.run_ok(&["status"]).stdout).unwrap();
    assert!(!status.contains("history"), "{status}");
}

/// A write started from a git hook of another repository inherits that
/// repository's variables; the commit goes to the graph's repository
/// alone, and the other one's history and index are as they were.
#[test]
fn a_write_with_git_dir_set_to_another_repository_commits_only_to_the_graph() {
    let project = shared();
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
    assert_eq!(graph_log(&graph(&project)), ["fmt", "hand edit", "init"]);
    assert_eq!(graph_log(other.path()), ["other"]);
    assert_eq!(fs::read(other_git.join("index")).unwrap(), index);
}

#[test]
fn a_write_while_git_holds_the_index_is_refused_and_writes_nothing() {
    let project = shared();
    let edited = hand_edit(&project);
    let lock = graph(&project).join(".git/index.lock");
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
    assert_eq!(graph_log(&graph(&project)), ["init"]);
}

/// `check` fails on a leftover index lock and warns about files the
/// history does not hold; `status` names both. Neither writes to the
/// repository: the index keeps its bytes.
#[test]
fn check_and_status_report_an_uncommitted_tree_and_a_leftover_index_lock() {
    let project = shared();
    hand_edit(&project);
    let lock = graph(&project).join(".git/index.lock");
    fs::write(&lock, "").unwrap();
    let index = graph(&project).join(".git/index");
    let before = fs::read(&index).unwrap();

    let stderr = project.run_refused(&["check"]);
    let status = String::from_utf8(project.run_ok(&["status"]).stdout).unwrap();

    let uncommitted = format!(
        "{}: 1 file(s) differ from the last commit of the graph's history: {EDITED};",
        graph(&project).display()
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
    assert!(!graph(&project).exists());

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
    let config = graph(&project).join(".git/config");
    let local = fs::read_to_string(&config).unwrap();
    fs::write(&config, format!("{local}{}", hostile.settings())).unwrap();
    project.write(".gitattributes", "* text eol=crlf ident filter=broken\n");
    let edited = decision("project", "A plan $Id$", "").replace('\n', "\r\n");
    project.write(EDITED, &edited);

    run(&["fmt"]);

    let graph = graph(&project);
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
/// place, its history commit made, its journal deleted. The next opener
/// finishes it, and the history holds its commit once.
#[cfg(feature = "failpoints")]
#[test]
fn a_write_stopped_after_its_commit_point_is_committed_once_by_the_next_opener() {
    use crate::harness::trurlic_ok;

    for spec in [
        "commit.placed:1",
        "history.committed:1",
        "commit.finished:1",
    ] {
        let project = shared();
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
            graph_log(&graph(&project)),
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

    let project = shared();
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
    let lock = graph(&project).join(".git/index.lock");
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
    assert_eq!(graph_log(&graph(&project)), ["fmt", "hand edit", "init"]);
}
