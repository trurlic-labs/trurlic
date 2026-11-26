//! The anchor findings of `status` and `check`: an anchor that names
//! nothing is stale on an accepted decision in `check`, which fails on it
//! as on an ambiguous one, planned on a proposed one, and unresolved in
//! `status`, which runs no git in the project; a weak anchor is a
//! warning. `status --section anchors` is the same JSON over MCP and from
//! the command line, the same bytes cold and warm, and is read in a tree
//! with no git. The pending findings of a linked worktree are
//! `placement.rs`'s, whose trees they need.

use std::fs;

use serde_json::{Value, json};

use crate::harness::{McpClient, Project, decision, git};

/// A decision of `project` anchored at `anchors`, each `file` or
/// `file::symbol`, accepted or proposed, in canonical form.
fn anchored(project: &Project, name: &str, anchors: &[&str], status: &str) {
    let spelled: Vec<String> = anchors
        .iter()
        .map(|anchor| match anchor.split_once("::") {
            Some((file, symbol)) => format!("{{ file = \"{file}\", symbol = \"{symbol}\" }}"),
            None => format!("{{ file = \"{anchor}\" }}"),
        })
        .collect();
    let extra = format!("anchors = [{}]\n", spelled.join(", "));
    let text = decision("project", &format!("Rule {name}"), &extra)
        .replace("status = \"accepted\"", &format!("status = \"{status}\""));
    project.write(&format!("decisions/{name}.toml"), &text);
    project.run_ok(&["fmt"]);
}

/// A project with a source tree: `src/lib.rs` defines `open`, a `Book`
/// with a field and a method `pages`, a `Shelf` with a field `pages`, and
/// a `Runner` with a method `run`, a name nine other files define as a
/// function.
fn project() -> Project {
    let project = Project::init();
    project.source(
        "src/lib.rs",
        "pub fn open() {}\npub struct Runner;\nimpl Runner { pub fn run(&self) {} }\n\
         pub struct Book { pages: u32 }\n\
         impl Book { pub fn pages(&self) -> u32 { self.pages } }\n\
         pub struct Shelf { pages: u32 }\n",
    );
    for n in 1..10 {
        project.source(&format!("src/m{n}.rs"), "pub fn run() {}\n");
    }
    project
}

fn anchors(client: &mut McpClient) -> Value {
    client.call_tool("status", json!({ "section": "anchors" }))
}

/// `status` lists each kind with its cause, the unresolved first, and
/// counts them; a missing anchor on a proposed decision is no finding
/// there, while a malformed or an unverified one is, whatever the status;
/// a field and its getter, one place, and a function named by its whole
/// path are none; the default report leaves the section out.
#[test]
fn status_lists_each_finding_with_its_kind_and_cause() {
    let project = project();
    let kinds = [
        "src/lib.rs::open",
        "src/lib.rs::close",
        "src/gone.rs",
        "src/lib.rs::pages",
        "src/lib.rs::Book::pages",
        "src/lib.rs::run",
        "src/m1.rs::run",
        "src/lib.rs::a::<B",
        "notes.txt::line",
    ];
    project.source("notes.txt", "line\n");
    anchored(&project, "kept", &kinds, "accepted");
    anchored(&project, "drafted", &kinds, "proposed");
    let (mut client, _) = McpClient::connect(&project);

    let status = anchors(&mut client);

    let missing = "its file defines nothing its symbol names in this tree";
    let ambiguous = "its symbol names definitions of 2 paths";
    let weak = "its symbol is one name, defined in 10 files of its kind";
    let malformed = "its symbol has an angle bracket that does not pair";
    let unverified =
        "its symbol is on a kind of file no resolver reads, so what it names is not verified";
    let item = |decision: &str, anchor: &str, kind: &str, cause: &str| json!({ "decision": decision, "anchor": anchor, "kind": kind, "cause": cause });
    assert_eq!(
        status["anchors"]["items"],
        json!([
            item(
                "kept",
                "src/gone.rs",
                "unresolved",
                "nothing is at its path in this tree"
            ),
            item("kept", "src/lib.rs::close", "unresolved", missing),
            item("drafted", "src/lib.rs::a::<B", "malformed", malformed),
            item("kept", "src/lib.rs::a::<B", "malformed", malformed),
            item("drafted", "src/lib.rs::pages", "ambiguous", ambiguous),
            item("kept", "src/lib.rs::pages", "ambiguous", ambiguous),
            item("drafted", "notes.txt::line", "unverified", unverified),
            item("kept", "notes.txt::line", "unverified", unverified),
            item("drafted", "src/lib.rs::run", "weak", weak),
            item("kept", "src/lib.rs::run", "weak", weak),
        ])
    );
    assert_eq!(
        status["counts"]["anchors"],
        json!({ "unresolved": 2, "malformed": 2, "ambiguous": 2, "unverified": 2, "weak": 2 })
    );
    let whole = client.call_tool("status", json!({}));
    assert!(whole.get("anchors").is_none(), "{whole}");
    assert!(whole["counts"].get("anchors").is_none(), "{whole}");
}

/// `origin` narrows the findings to the decisions recorded under it.
#[test]
fn an_origin_narrows_the_findings_to_its_decisions() {
    let project = project();
    anchored(&project, "kept", &["src/gone.rs"], "accepted");
    anchored(&project, "drafted", &["src/gone.rs"], "accepted");
    let text = project.read("decisions/drafted.toml");
    project.write(
        "decisions/drafted.toml",
        &format!("{text}origin = \"row 1\"\n"),
    );
    let (mut client, _) = McpClient::connect(&project);

    let narrowed = client.call_tool("status", json!({ "section": "anchors", "origin": "row 1" }));

    let items = narrowed["anchors"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{narrowed}");
    assert_eq!(items[0]["decision"], "drafted");
    assert_eq!(narrowed["counts"]["anchors"]["unresolved"], 1);
}

/// `check` fails on a stale, a malformed or an ambiguous anchor, each an
/// error, a malformed one on a proposed decision too: an unverified or a
/// weak one is a warning, a planned one a note, and none of those fails
/// it. A pending one, a note too, is `placement.rs`'s.
#[test]
fn check_fails_on_a_stale_or_an_ambiguous_anchor() {
    let cases = [
        (
            "src/gone.rs",
            "accepted",
            Some(1),
            "  error: anchor `src/gone.rs` of `d` is stale: nothing is at its path\n",
        ),
        (
            "src/gone.rs",
            "proposed",
            Some(0),
            "  note: anchor `src/gone.rs` of `d` is planned: nothing is at its path\n",
        ),
        (
            "src/lib.rs::pages",
            "accepted",
            Some(1),
            "  error: anchor `src/lib.rs::pages` of `d` is ambiguous: its symbol names definitions of 2 paths\n",
        ),
        (
            "src/lib.rs::run",
            "accepted",
            Some(0),
            "  warning: anchor `src/lib.rs::run` of `d` is weak: its symbol is one name, defined in 10 files of its kind\n",
        ),
        (
            "src/lib.rs::a::<B",
            "proposed",
            Some(1),
            "  error: anchor `src/lib.rs::a::<B` of `d` is malformed: its symbol has an angle bracket that does not pair\n",
        ),
        (
            "notes.txt::line",
            "accepted",
            Some(0),
            "  warning: anchor `notes.txt::line` of `d` is unverified: its symbol is on a kind of file no resolver reads, so what it names is not verified\n",
        ),
        ("src/lib.rs::open", "accepted", Some(0), ""),
    ];
    for (anchor, status, code, line) in cases {
        let project = project();
        project.source("notes.txt", "line\n");
        anchored(&project, "d", &[anchor], status);

        let output = project.command().arg("check").output().unwrap();

        let stderr = String::from_utf8(output.stderr).unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert_eq!(output.status.code(), code, "{anchor}: {stderr}");
        assert!(stderr.starts_with(line), "{anchor}: {stderr}");
        let consistent = if line.is_empty() {
            ".trurlic/ is consistent\n"
        } else {
            ""
        };
        assert_eq!(stdout, consistent, "{anchor}");
    }
    // A proposed revision of an accepted decision has all its anchors
    // judged as accepted, since the accepted text keeps none: stale.
    let revised = project();
    anchored(&revised, "d", &["src/gone.rs"], "proposed");
    let text = revised.read("decisions/d.toml");
    let accepted = "\n[accepted_text]\nchoice = \"Rule d\"\nreason = \"The accepted reason\"\n";
    revised.write("decisions/d.toml", &format!("{text}{accepted}"));
    let stderr = revised.run_refused(&["check"]);
    assert!(stderr.contains("`src/gone.rs` of `d` is stale"), "{stderr}");
    let project = project();
    anchored(&project, "d", &["src/gone.rs"], "accepted");
    let stderr = project.run_refused(&["check"]);
    assert!(
        stderr.ends_with("error: 1 consistency error(s) found\n"),
        "{stderr}"
    );
}

/// The section over MCP and `status --section anchors --json` print the
/// same text, and the command prints a line per finding for a terminal.
#[test]
fn the_anchors_section_is_the_same_from_the_command_line() {
    let project = project();
    anchored(
        &project,
        "d",
        &["src/gone.rs", "src/lib.rs::run"],
        "accepted",
    );
    let (mut client, _) = McpClient::connect(&project);

    let tool = client.call_tool_text("status", json!({ "section": "anchors" }));
    let command = project
        .command()
        .args(["status", "--json", "--section", "anchors"])
        .output()
        .unwrap();
    let text = project
        .command()
        .args(["status", "--section", "anchors"])
        .output()
        .unwrap();

    assert_eq!(
        String::from_utf8(command.stdout).unwrap(),
        format!("{tool}\n")
    );
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(
        text.contains(
            "anchors: 1 unresolved, 0 malformed, 0 ambiguous, 0 unverified, 1 weak\n  \
             unresolved: `src/gone.rs` of `d`: nothing is at its path in this tree\n  \
             weak: `src/lib.rs::run` of `d`: its symbol is one name, defined in 10 files of its kind\n"
        ),
        "{text}"
    );
}

/// The first call indexes the tree and fills the cache; the next reads
/// it, and both answer the same bytes.
#[test]
fn a_cold_and_a_warm_cache_give_the_same_bytes() {
    let project = project();
    anchored(
        &project,
        "d",
        &["src/lib.rs::close", "src/lib.rs::run", "src/lib.rs::pages"],
        "accepted",
    );
    let cache = project.node(".state/symbols");
    let section = || {
        let output = project
            .command()
            .args(["status", "--json", "--section", "anchors"])
            .output()
            .unwrap();
        String::from_utf8(output.stdout).unwrap()
    };

    assert!(!cache.exists());
    let cold = section();
    let entries = fs::read_dir(&cache).unwrap().count();
    let warm = section();

    // `src/lib.rs`, and the nine files of one content as one entry.
    assert_eq!(entries, 2);
    assert_eq!(cold, warm);
    assert!(cold.contains("\"unresolved\":1"), "{cold}");
}

/// A tree in no git work tree is judged by the same rules, its anchors
/// asked of no other worktree.
#[test]
fn a_tree_with_no_git_is_judged_alike() {
    let project = Project::shared();
    project.source("src/lib.rs", "pub fn open() {}\n");
    anchored(
        &project,
        "kept",
        &["src/lib.rs::close", "src/lib.rs::open"],
        "accepted",
    );
    anchored(&project, "drafted", &["src/lib.rs::close"], "proposed");
    let (mut client, _) = McpClient::connect(&project);

    let status = anchors(&mut client);

    assert_eq!(
        status["counts"]["anchors"],
        json!({ "unresolved": 1, "malformed": 0, "ambiguous": 0, "unverified": 0, "weak": 0 })
    );
    let stderr = project.run_refused(&["check"]);
    assert!(
        stderr.contains("`src/lib.rs::close` of `kept` is stale"),
        "{stderr}"
    );
}

/// On a terminal the section prints its count line even with no finding,
/// so a clean tree is told from a section not asked for.
#[test]
fn the_count_line_is_printed_with_no_finding() {
    let project = project();
    anchored(&project, "d", &["src/lib.rs::open"], "accepted");

    let text = project.run_ok(&["status", "--section", "anchors"]).stdout;
    let whole = project.run_ok(&["status"]).stdout;

    let line = "anchors: 0 unresolved, 0 malformed, 0 ambiguous, 0 unverified, 0 weak\n";
    assert!(String::from_utf8(text).unwrap().contains(line));
    assert!(!String::from_utf8(whole).unwrap().contains("anchors:"));
}

/// `status` runs no git in the project: with no `git` on `PATH` its
/// section names a missing anchor of a tracked graph unresolved, while
/// `check`, which runs git, cannot run.
#[test]
fn status_asks_the_project_s_git_nothing() {
    let project = project();
    anchored(&project, "d", &["src/gone.rs"], "accepted");
    let empty = tempfile::TempDir::new().unwrap();
    let no_git = |args: &[&str]| {
        project
            .command()
            .args(args)
            .env("PATH", empty.path())
            .output()
            .unwrap()
    };

    let status = no_git(&["status", "--json", "--section", "anchors"]);
    let check = no_git(&["check"]);

    assert!(status.status.success(), "{status:?}");
    let report: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(report["counts"]["anchors"]["unresolved"], 1, "{report}");
    assert_eq!(check.status.code(), Some(1));
    let stderr = String::from_utf8(check.stderr).unwrap();
    assert!(stderr.contains("cannot run `git`"), "{stderr}");
}

/// When git cannot say whether another worktree is ahead (its HEAD names
/// a commit the repository lacks), `check` warns that it went unread
/// beside the stale anchor that may be pending there; `status`, which
/// never runs it, carries no such notice.
#[test]
fn a_worktree_git_cannot_say_enough_of_is_a_check_warning() {
    let project = project();
    anchored(&project, "d", &["src/gone.rs"], "accepted");
    git(project.path(), &["add", "--all"]);
    git(project.path(), &["commit", "--quiet", "-m", "anchored"]);
    let other = tempfile::TempDir::new().unwrap();
    let linked = other.path().canonicalize().unwrap().join("linked");
    let to = linked.to_str().unwrap();
    git(
        project.path(),
        &["worktree", "add", "--quiet", "--detach", to],
    );
    let head = project.path().join(".git/worktrees/linked/HEAD");
    fs::write(head, format!("{}\n", "1".repeat(40))).unwrap();
    let (mut client, _) = McpClient::connect(&project);

    let status = anchors(&mut client);
    let check = project.command().arg("check").output().unwrap();

    assert_eq!(status["counts"]["anchors"]["unresolved"], 1, "{status}");
    assert!(status.get("unread_worktrees").is_none(), "{status}");
    let stderr = String::from_utf8(check.stderr).unwrap();
    assert_eq!(check.status.code(), Some(1), "{stderr}");
    let unread = format!(
        "  warning: the worktree at {} was not read for pending anchors: ",
        linked.display()
    );
    assert!(
        stderr
            .starts_with("  error: anchor `src/gone.rs` of `d` is stale: nothing is at its path\n"),
        "{stderr}"
    );
    assert!(stderr.contains(&unread), "{stderr}");
}

/// A git that prints without end is killed past the bytes a git command's
/// output is read to, and the call fails naming them: `check`, which lists
/// the worktrees, fails with that error rather than hang or fill memory.
#[cfg(unix)]
#[test]
fn a_git_that_prints_without_end_is_cut_off_at_the_stated_bytes() {
    let project = project();
    anchored(&project, "d", &["src/gone.rs"], "accepted");
    git(project.path(), &["add", "--all"]);
    git(project.path(), &["commit", "--quiet", "-m", "anchored"]);
    let tools = tempfile::TempDir::new().unwrap();
    let script = "case \" $* \" in *\" worktree list \"*) exec cat /dev/zero ;; esac\n\
                  exec \"$REAL\" \"$@\"";
    let path = crate::harness::fake_git(tools.path(), script);

    let check = project
        .command()
        .arg("check")
        .env("PATH", &path)
        .output()
        .unwrap();

    let stderr = String::from_utf8(check.stderr).unwrap();
    assert_eq!(check.status.code(), Some(1), "{stderr}");
    let cut = format!(
        "`git worktree list --porcelain -z` printed more than {} bytes",
        256 * 1024 * 1024
    );
    assert!(stderr.contains(&cut), "{stderr}");
}

/// `origin` narrows which decisions are judged, never the verdict on an
/// anchor: a file only another origin's decision anchors, which the walk
/// passes over, is not counted for weak with or without it.
#[test]
fn an_origin_leaves_the_verdict_on_an_anchor_as_it_is() {
    let project = project();
    fs::remove_file(project.path().join("src/m9.rs")).unwrap();
    project.source(".hidden/x.rs", "pub fn run() {}\n");
    anchored(&project, "kept", &["src/lib.rs::run"], "accepted");
    anchored(&project, "other", &[".hidden/x.rs::run"], "accepted");
    let text = project.read("decisions/kept.toml");
    project.write(
        "decisions/kept.toml",
        &format!("{text}origin = \"row 1\"\n"),
    );
    let (mut client, _) = McpClient::connect(&project);

    let whole = anchors(&mut client);
    let narrowed = client.call_tool("status", json!({ "section": "anchors", "origin": "row 1" }));

    assert_eq!(whole["anchors"]["items"], json!([]), "{whole}");
    assert_eq!(narrowed["anchors"]["items"], json!([]), "{narrowed}");
}
