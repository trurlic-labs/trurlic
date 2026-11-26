//! Placement and discovery over real git repositories: a main worktree, a
//! linked worktree, and a clone outside the repository that reaches the
//! graph through a `.trurlic` symlink. `serve`, `status` and `check`
//! started in each find the graph and read it for their own tree; `check`
//! refuses each placement git contradicts and a second graph; `init`
//! refuses a placement git cannot hold; and writers started in different
//! trees of one graph take one lock, so servers in each tree that apply
//! batches at once lose none. Unix only, for the symlinks.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::json;
use tempfile::TempDir;

use crate::harness::{
    McpClient, Project, decision, git, graph_log, record, trurlic, trurlic_ok, trurlic_refused,
};

/// A main worktree at `main` with a graph holding the component `auth`
/// and the decision `sign-tokens`, anchored at `src/branch.rs`, a file only
/// the linked worktree's branch has, committed there; a linked worktree at `linked` on branch
/// `feature`; and, under a shared graph, a clone at `clone` whose
/// `.trurlic` is a symlink to the main worktree's graph. Every path is
/// canonical.
struct Trees {
    _tmp: TempDir,
    root: PathBuf,
    main: PathBuf,
    linked: PathBuf,
    clone: PathBuf,
}

impl Trees {
    fn shared() -> Self {
        let trees = Self::repository();
        trurlic_ok(&trees.main, &["init", "--placement", "shared"]);
        trees.write_graph();
        trees.add_linked();
        git(&trees.root, &["clone", "--quiet", "main", "clone"]);
        symlink(Path::new("../main/.trurlic"), &trees.clone.join(".trurlic"));
        trees
    }

    /// The graph is committed before the linked worktree is added, so the
    /// worktree checks out a graph of its own.
    fn tracked() -> Self {
        let trees = Self::repository();
        trurlic_ok(&trees.main, &["init", "--placement", "tracked"]);
        trees.write_graph();
        git(&trees.main, &["add", "--all"]);
        git(&trees.main, &["commit", "--quiet", "-m", "graph"]);
        trees.add_linked();
        trees
    }

    /// `main`, a repository on branch `main` with one commit.
    fn repository() -> Self {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let main = root.join("main");
        fs::create_dir_all(main.join("src")).unwrap();
        git(&main, &["init", "--quiet", "--initial-branch=main"]);
        fs::write(main.join("src/lib.rs"), "").unwrap();
        git(&main, &["add", "--all"]);
        git(&main, &["commit", "--quiet", "-m", "first"]);
        Self {
            _tmp: tmp,
            linked: root.join("linked"),
            clone: root.join("clone"),
            main,
            root,
        }
    }

    fn write_graph(&self) {
        let graph = self.main.join(".trurlic");
        fs::write(
            graph.join("components/auth.toml"),
            "description = \"Auth\"\n",
        )
        .unwrap();
        fs::write(
            graph.join("decisions/sign-tokens.toml"),
            decision(
                "auth",
                "Sign tokens",
                "anchors = [\n    { file = \"src/branch.rs\" },\n]\n",
            ),
        )
        .unwrap();
        trurlic_ok(&self.main, &["fmt"]);
    }

    fn add_linked(&self) {
        git(
            &self.main,
            &["worktree", "add", "--quiet", "-b", "feature", "../linked"],
        );
        fs::write(self.linked.join("src/branch.rs"), "").unwrap();
        git(&self.linked, &["add", "src/branch.rs"]);
        git(&self.linked, &["commit", "--quiet", "-m", "branch"]);
    }

    fn graph(&self) -> PathBuf {
        self.main.join(".trurlic")
    }
}

fn symlink(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}

/// `get_context("auth")` from a server started in `dir`: the component's
/// description and how many of its decisions anchor only files the tree
/// lacks.
fn context_from(dir: &Path) -> (String, u64) {
    let (mut client, _) = McpClient::connect_in(dir, &[], json!({}));
    let context = client.call_tool("get_context", json!({ "component": "auth" }));
    (
        context["component"]["description"]
            .as_str()
            .unwrap()
            .to_owned(),
        context["health"]["orphaned_anchors"].as_u64().unwrap(),
    )
}

/// The main worktree's shared graph is found from the main worktree, the
/// linked worktree and the symlinked clone, and from a directory inside
/// each, and each reads it for its own tree: the decision's anchored file
/// exists only in the linked worktree, so only there is it not orphaned,
/// `check` finds it pending on the linked worktree's branch from the main
/// worktree, and stale in the clone, whose repository has no other
/// worktree.
#[test]
fn every_tree_finds_the_shared_graph_and_reads_it_for_itself() {
    let trees = Trees::shared();
    let graph = trees.graph();
    let stale = "  error: anchor `src/branch.rs` of `sign-tokens` is stale: nothing is at its \
                 path\nerror: 1 consistency error(s) found\n";
    let consistent = format!("{}/ is consistent\n", graph.display());
    let cases = [
        (
            &trees.main,
            1,
            (Some(0), String::new(), pending_on_feature(&trees)),
        ),
        (&trees.linked, 0, (Some(0), consistent, String::new())),
        (&trees.clone, 1, (Some(1), String::new(), stale.to_owned())),
    ];
    for (tree, orphaned, check) in cases {
        for start in [tree.clone(), tree.join("src")] {
            let status = trurlic_ok(&start, &["status"]);
            assert!(status.contains("components: 1\n"), "{status}");
            assert!(status.contains("decisions: 1 "), "{status}");
            let shown = format!(
                "graph: {}\ncode root: {}\n",
                graph.display(),
                tree.display()
            );
            assert_eq!(status.contains(&shown), *tree != trees.main, "{status}");

            assert_eq!(checked(&start), check, "{}", start.display());
            assert_eq!(context_from(&start), ("Auth".to_owned(), orphaned));
        }
    }
}

/// What `check` in `dir` exits with and prints on stdout and stderr.
fn checked(dir: &Path) -> (Option<i32>, String, String) {
    let output = trurlic(dir).arg("check").output().unwrap();
    (
        output.status.code(),
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

/// The note `check` prints of the decision's anchor in a tree that lacks
/// the file the linked worktree holds.
fn pending_on_feature(trees: &Trees) -> String {
    format!(
        "  note: anchor `src/branch.rs` of `sign-tokens` is pending: nothing is at its path \
         here; it resolves in the worktree at {} on branch `feature`\n",
        trees.linked.display()
    )
}

/// Under `tracked` the linked worktree checks out a graph of its own,
/// which the walk finds: an edit of the main worktree's graph is not seen
/// there.
#[test]
fn a_linked_worktree_of_a_tracked_graph_reads_its_own() {
    let trees = Trees::tracked();
    fs::write(
        trees.graph().join("components/auth.toml"),
        "description = \"Edited in main\"\n",
    )
    .unwrap();

    let status = trurlic_ok(&trees.linked, &["status"]);
    assert!(!status.contains("graph: "), "{status}");
    assert_eq!(context_from(&trees.linked), ("Auth".to_owned(), 0));
    assert_eq!(context_from(&trees.main), ("Edited in main".to_owned(), 1));
    assert_eq!(
        checked(&trees.main),
        (Some(0), String::new(), pending_on_feature(&trees))
    );
    assert_eq!(
        trurlic_ok(&trees.linked, &["check"]),
        ".trurlic/ is consistent\n"
    );
}

// -- an anchor another worktree holds is pending ----------------------------

/// Under either placement, a symbol only the linked worktree's branch
/// defines is pending on that branch in `check` from the main worktree,
/// which exits 0, and unresolved in `status`, which runs no git and
/// so only says it does not resolve in this tree; it resolves in the
/// linked one. With the linked worktree's HEAD detached it is pending on
/// no branch; with its directory gone, git marks it prunable and `check`
/// fails on it, stale, while `status` says what it said.
#[test]
fn a_symbol_only_the_branch_defines_is_pending_on_its_branch() {
    for trees in [Trees::tracked(), Trees::shared()] {
        let anchor = "anchors = [\n    { file = \"src/lib.rs\", symbol = \"only_here\" },\n]\n";
        fs::write(
            trees.graph().join("decisions/branch-rule.toml"),
            decision("auth", "Branch rule", anchor),
        )
        .unwrap();
        trurlic_ok(&trees.main, &["fmt"]);
        if trees.linked.join(".trurlic").is_dir() {
            fs::copy(
                trees.graph().join("decisions/branch-rule.toml"),
                trees.linked.join(".trurlic/decisions/branch-rule.toml"),
            )
            .unwrap();
        }
        fs::write(trees.linked.join("src/lib.rs"), "pub fn only_here() {}\n").unwrap();
        git(
            &trees.linked,
            &["commit", "--quiet", "-a", "-m", "only here"],
        );
        let (mut client, _) = McpClient::connect_in(&trees.main, &[], json!({}));

        let status = client.call_tool("status", json!({ "section": "anchors" }));

        let rule = |items: &serde_json::Value| {
            items["anchors"]["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["decision"] == "branch-rule")
                .cloned()
        };
        let unresolved = json!({
            "decision": "branch-rule", "anchor": "src/lib.rs::only_here", "kind": "unresolved",
            "cause": "its file defines nothing its symbol names in this tree",
        });
        assert_eq!(rule(&status), Some(unresolved.clone()));
        let missing = "its file defines nothing its symbol names here";
        let (code, _, stderr) = checked(&trees.main);
        let pending = format!(
            "  note: anchor `src/lib.rs::only_here` of `branch-rule` is pending: {missing}; it \
             resolves in the worktree at {} on branch `feature`\n",
            trees.linked.display()
        );
        assert_eq!(code, Some(0), "{stderr}");
        assert!(stderr.contains(&pending), "{stderr}");
        let (mut there, _) = McpClient::connect_in(&trees.linked, &[], json!({}));
        let from_linked = there.call_tool("status", json!({ "section": "anchors" }));
        assert_eq!(rule(&from_linked), None, "{from_linked}");

        git(&trees.linked, &["checkout", "--quiet", "--detach"]);
        let (code, _, stderr) = checked(&trees.main);
        assert_eq!(code, Some(0), "{stderr}");
        assert!(
            stderr.contains("`src/lib.rs::only_here` of `branch-rule` is pending")
                && stderr.contains(", whose HEAD is detached\n"),
            "{stderr}"
        );

        fs::remove_dir_all(&trees.linked).unwrap();
        let gone = client.call_tool("status", json!({ "section": "anchors" }));
        assert_eq!(rule(&gone), Some(unresolved), "{gone}");
        let (code, _, stderr) = checked(&trees.main);
        assert_eq!(code, Some(1), "{stderr}");
        assert!(
            stderr.contains("`src/lib.rs::only_here` of `branch-rule` is stale"),
            "{stderr}"
        );
    }
}

/// A repository whose `main` branch at `dir` holds `src/lib.rs` defining
/// `foo` and a tracked graph whose accepted decision `rule` anchors it.
fn anchored_repository(dir: &Path) {
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(dir.join("src/lib.rs"), "pub fn foo() {}\n").unwrap();
    git(dir, &["init", "--quiet", "--initial-branch=main"]);
    trurlic_ok(dir, &["init", "--placement", "tracked"]);
    let anchor = "anchors = [\n    { file = \"src/lib.rs\", symbol = \"foo\" },\n]\n";
    fs::write(
        dir.join(".trurlic/decisions/rule.toml"),
        decision("project", "A rule", anchor),
    )
    .unwrap();
    trurlic_ok(dir, &["fmt"]);
    git(dir, &["add", "--all"]);
    git(dir, &["commit", "--quiet", "-m", "first"]);
}

/// A worktree of a branch the calling tree has moved past, its work tree
/// as committed, holds nothing the calling tree lacks: an anchor deleted
/// since is stale, not pending on the old branch. Nor does one of a branch
/// cut before the deletion with a commit of its own, which never changed
/// the anchor's file: merging it would not bring the code back.
#[test]
fn a_worktree_that_did_not_change_the_file_since_the_fork_makes_nothing_pending() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let main = root.join("main");
    anchored_repository(&main);
    git(
        &main,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "release-1",
            "../release",
        ],
    );
    git(
        &main,
        &["worktree", "add", "--quiet", "-b", "feature", "../feature"],
    );
    fs::write(root.join("feature/src/other.rs"), "pub fn other() {}\n").unwrap();
    git(&root.join("feature"), &["add", "src/other.rs"]);
    git(&root.join("feature"), &["commit", "--quiet", "-m", "other"]);
    fs::write(main.join("src/lib.rs"), "").unwrap();
    git(&main, &["commit", "--quiet", "-a", "-m", "drop foo"]);

    let (code, _, stderr) = checked(&main);

    assert_eq!(code, Some(1), "{stderr}");
    assert!(
        stderr.starts_with("  error: anchor `src/lib.rs::foo` of `rule` is stale: its file defines nothing its symbol names\n"),
        "{stderr}"
    );
    assert!(!stderr.contains("pending"), "{stderr}");
}

/// A deletion the calling tree committed is never hidden by a worktree
/// whose line forked before it and still holds the code, whatever else
/// that worktree changed in the file: an edit not committed, or a commit
/// of its own made before the deletion. The anchor is stale there.
#[test]
fn a_worktree_that_edited_the_file_elsewhere_hides_no_deletion() {
    for committed in [false, true] {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let main = root.join("main");
        anchored_repository(&main);
        git(
            &main,
            &["worktree", "add", "--quiet", "-b", "fix", "../fix"],
        );
        let fix = root.join("fix");
        fs::write(fix.join("src/lib.rs"), "/// Reworded.\npub fn foo() {}\n").unwrap();
        if committed {
            git(&fix, &["commit", "--quiet", "-a", "-m", "reword"]);
        }
        fs::write(main.join("src/lib.rs"), "").unwrap();
        git(
            &main,
            &["commit", "--quiet", "-a", "-m", "drop foo for good"],
        );

        let (code, _, stderr) = checked(&main);

        assert_eq!(code, Some(1), "committed: {committed}: {stderr}");
        assert!(
            stderr.starts_with(
                "  error: anchor `src/lib.rs::foo` of `rule` is stale: its file defines nothing its symbol names\n"
            ),
            "committed: {committed}: {stderr}"
        );
        assert!(!stderr.contains("pending"), "{stderr}");
    }
}

/// A submodule's main worktree, which git lists by its git directory, is
/// the one that directory's configuration names: from it, its linked
/// worktree is read, so a symbol written there is pending; and from the
/// linked worktree, it is read, so a symbol written in it is pending.
#[test]
fn a_submodule_s_main_and_linked_worktrees_read_each_other() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    anchored_repository(&root.join("lib"));
    let sup = root.join("sup");
    fs::create_dir_all(&sup).unwrap();
    git(&sup, &["init", "--quiet", "--initial-branch=main"]);
    git(
        &sup,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "--quiet",
            "../lib",
            "lib",
        ],
    );
    git(&sup, &["commit", "--quiet", "-m", "submodule"]);
    let lib = sup.join("lib");
    let anchor = "anchors = [\n    { file = \"src/lib.rs\", symbol = \"newfn\" },\n    \
                  { file = \"src/lib.rs\", symbol = \"inmain\" },\n]\n";
    fs::write(
        lib.join(".trurlic/decisions/later.toml"),
        decision("project", "A later rule", anchor),
    )
    .unwrap();
    trurlic_ok(&lib, &["fmt"]);
    git(&lib, &["add", "--all"]);
    git(&lib, &["commit", "--quiet", "-m", "later rule"]);
    git(
        &lib,
        &["worktree", "add", "--quiet", "-b", "s2", "../../subwt"],
    );
    let linked = root.join("subwt");
    fs::write(
        linked.join("src/lib.rs"),
        "pub fn foo() {}\npub fn newfn() {}\n",
    )
    .unwrap();
    fs::write(
        lib.join("src/lib.rs"),
        "pub fn foo() {}\npub fn inmain() {}\n",
    )
    .unwrap();

    let (main_code, _, from_main) = checked(&lib);
    let (linked_code, _, from_linked) = checked(&linked);

    let pending = |symbol: &str, at: &Path| {
        format!(
            "`src/lib.rs::{symbol}` of `later` is pending: its file defines nothing its symbol \
             names here; it resolves in the worktree at {}",
            at.display()
        )
    };
    assert_eq!(main_code, Some(0), "{from_main}");
    assert!(
        from_main.contains(&pending("newfn", &linked)),
        "{from_main}"
    );
    assert_eq!(linked_code, Some(0), "{from_linked}");
    assert!(
        from_linked.contains(&pending("inmain", &lib)),
        "{from_linked}"
    );
    assert!(!from_linked.contains("warning"), "{from_linked}");
}

/// Code another worktree holds and has not committed counts by the one
/// rule, its work tree against the fork, whatever that worktree's commit:
/// at the calling tree's commit with an edit, behind it with an edit, and
/// ahead with a new file not yet added each make their anchor pending.
#[test]
fn code_another_worktree_has_not_committed_is_pending_there() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let main = root.join("main");
    anchored_repository(&main);
    let anchors = "anchors = [\n    { file = \"src/lib.rs\", symbol = \"same\" },\n    \
                   { file = \"src/lib.rs\", symbol = \"behind\" },\n    \
                   { file = \"src/new.rs\", symbol = \"untracked\" },\n]\n";
    fs::write(
        main.join(".trurlic/decisions/later.toml"),
        decision("project", "Later rules", anchors),
    )
    .unwrap();
    trurlic_ok(&main, &["fmt"]);
    for (branch, dir) in [("behind", "../behind"), ("ahead", "../ahead")] {
        git(&main, &["worktree", "add", "--quiet", "-b", branch, dir]);
    }
    git(&main, &["add", "--all"]);
    git(&main, &["commit", "--quiet", "-m", "later rules"]);
    git(
        &main,
        &["worktree", "add", "--quiet", "-b", "same", "../same"],
    );
    let ahead = root.join("ahead");
    fs::write(ahead.join("src/other.rs"), "pub fn other() {}\n").unwrap();
    git(&ahead, &["add", "src/other.rs"]);
    git(&ahead, &["commit", "--quiet", "-m", "other"]);
    let written = [
        ("same/src/lib.rs", "pub fn foo() {}\npub fn same() {}\n"),
        ("behind/src/lib.rs", "pub fn foo() {}\npub fn behind() {}\n"),
        ("ahead/src/new.rs", "pub fn untracked() {}\n"),
    ];
    for (path, text) in written {
        fs::write(root.join(path), text).unwrap();
    }

    let (code, _, stderr) = checked(&main);

    assert_eq!(code, Some(0), "{stderr}");
    let edited = "its file defines nothing its symbol names here";
    for (anchor, cause, branch) in [
        ("src/lib.rs::same", edited, "same"),
        ("src/lib.rs::behind", edited, "behind"),
        (
            "src/new.rs::untracked",
            "nothing is at its path here",
            "ahead",
        ),
    ] {
        let pending = format!(
            "`{anchor}` of `later` is pending: {cause}; it resolves in the worktree at {} on \
             branch `{branch}`",
            root.join(branch).display()
        );
        assert!(stderr.contains(&pending), "{pending}\n{stderr}");
    }
}

/// A linked worktree moved without `git worktree repair` is no live
/// worktree git lists at or above its tree: `check` there warns that the
/// other worktrees went unread, beside the anchor it then calls stale,
/// which was pending before the move.
#[test]
fn a_worktree_git_does_not_list_warns_that_the_others_went_unread() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let main = root.join("main");
    anchored_repository(&main);
    fs::write(main.join("src/lib.rs"), "").unwrap();
    git(&main, &["commit", "--quiet", "-a", "-m", "drop foo"]);
    for (branch, dir) in [("feature", "../feature"), ("other", "../other")] {
        git(&main, &["worktree", "add", "--quiet", "-b", branch, dir]);
    }
    fs::write(
        root.join("feature/src/lib.rs"),
        "pub fn foo() {}\npub fn bar() {}\n",
    )
    .unwrap();
    git(
        &root.join("feature"),
        &["commit", "--quiet", "-a", "-m", "foo again"],
    );
    fs::write(root.join("other/src/lib.rs"), "pub fn other() {}\n").unwrap();
    git(
        &root.join("other"),
        &["commit", "--quiet", "-a", "-m", "other"],
    );

    let (before, _, pending) = checked(&root.join("other"));
    fs::rename(root.join("other"), root.join("moved")).unwrap();
    let (after, _, stderr) = checked(&root.join("moved"));

    assert_eq!(before, Some(0), "{pending}");
    assert!(pending.contains("is pending"), "{pending}");
    assert_eq!(after, Some(1), "{stderr}");
    assert!(
        stderr.contains("`src/lib.rs::foo` of `rule` is stale")
            && stderr.contains(
                "  warning: the other worktrees were not read for pending anchors: git lists no \
                 live worktree at or above"
            ),
        "{stderr}"
    );
}

/// A graph in a subdirectory of its work tree is read against that
/// subdirectory in every worktree: alone in its repository, an anchor
/// the top of the tree defines and the subdirectory does not is stale;
/// a worktree whose branch is ahead and defines it in the same
/// subdirectory makes it pending there.
#[test]
fn a_graph_in_a_subdirectory_is_read_at_the_same_place_in_every_worktree() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let repository = root.join("repository");
    fs::create_dir_all(repository.join("src")).unwrap();
    fs::write(repository.join("src/lib.rs"), "pub fn foo() {}\n").unwrap();
    git(&repository, &["init", "--quiet", "--initial-branch=main"]);
    git(&repository, &["add", "--all"]);
    git(&repository, &["commit", "--quiet", "-m", "top"]);
    let sub = repository.join("sub");
    fs::create_dir_all(sub.join("src")).unwrap();
    fs::write(sub.join("src/lib.rs"), "pub fn bar() {}\n").unwrap();
    anchored_sub(&sub);

    let (code, _, stderr) = checked(&sub);
    assert_eq!(code, Some(1), "{stderr}");
    assert!(
        stderr.contains("`src/lib.rs::foo` of `rule` is stale"),
        "{stderr}"
    );

    git(
        &repository,
        &["worktree", "add", "--quiet", "-b", "ahead", "../linked"],
    );
    let linked = root.join("linked");
    fs::write(linked.join("sub/src/lib.rs"), "pub fn foo() {}\n").unwrap();
    git(&linked, &["commit", "--quiet", "-a", "-m", "foo"]);

    let (code, _, stderr) = checked(&sub);
    assert_eq!(code, Some(0), "{stderr}");
    let pending = format!(
        "is pending: its file defines nothing its symbol names here; it resolves in the \
         worktree at {} on branch `ahead`",
        linked.join("sub").display()
    );
    assert!(stderr.contains(&pending), "{stderr}");
}

/// A tracked graph at `sub`, a subdirectory of a work tree, whose accepted
/// decision `rule` anchors `src/lib.rs::foo`, committed.
fn anchored_sub(sub: &Path) {
    trurlic_ok(sub, &["init", "--placement", "tracked"]);
    let anchor = "anchors = [\n    { file = \"src/lib.rs\", symbol = \"foo\" },\n]\n";
    fs::write(
        sub.join(".trurlic/decisions/rule.toml"),
        decision("project", "A rule", anchor),
    )
    .unwrap();
    trurlic_ok(sub, &["fmt"]);
    git(sub, &["add", "--all"]);
    git(sub, &["commit", "--quiet", "-m", "sub"]);
}

// -- the walk stays in the work tree it starts in ---------------------------

impl Trees {
    /// A linked worktree nested in the main worktree at
    /// `.worktrees/nested`, on a new branch from `commit`, with the file the
    /// decision anchors; the main worktree's git ignores the directory.
    fn add_nested(&self, commit: &str) -> PathBuf {
        let exclude = self.main.join(".git/info/exclude");
        let held = fs::read_to_string(&exclude).unwrap_or_default();
        fs::write(&exclude, format!("{held}\n/.worktrees/\n")).unwrap();
        git(
            &self.main,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "nested",
                ".worktrees/nested",
                commit,
            ],
        );
        let nested = self.main.join(".worktrees/nested");
        fs::create_dir_all(nested.join("src")).unwrap();
        fs::write(nested.join("src/branch.rs"), "").unwrap();
        nested
    }
}

/// Under `shared`, a linked worktree inside the main worktree reads the
/// main worktree's graph for itself, as a sibling does: its code root is
/// its own top, from there and from a directory inside it, and its write
/// lands in the one graph.
#[test]
fn a_nested_worktree_of_a_shared_graph_reads_it_for_itself() {
    let trees = Trees::shared();
    let nested = trees.add_nested("HEAD");
    let graph = trees.graph();

    for start in [nested.clone(), nested.join("src")] {
        let status = trurlic_ok(&start, &["status"]);
        let shown = format!(
            "graph: {}\ncode root: {}\n",
            graph.display(),
            nested.display()
        );
        assert!(status.contains(&shown), "{status}");
        assert_eq!(context_from(&start), ("Auth".to_owned(), 0));
    }
    let batch = nested.join("batch.json");
    let ops = json!({ "origin": "nested", "ops": [record("from-nested", "auth", "A rule")] });
    fs::write(&batch, ops.to_string()).unwrap();
    trurlic_ok(&nested.join("src"), &["apply", batch.to_str().unwrap()]);
    assert!(graph.join("decisions/from-nested.toml").is_file());
}

/// `check` in a linked worktree nested in the main one reads every other
/// worktree at its own place in each: its own worktree is the nearest
/// listed top above it, not the main worktree that also holds it, so the
/// file it lacks is pending on the branch that holds it.
#[test]
fn a_nested_worktree_finds_what_a_sibling_holds_pending() {
    let trees = Trees::shared();
    let nested = trees.add_nested("HEAD");
    fs::remove_file(nested.join("src/branch.rs")).unwrap();

    let (code, _, stderr) = checked(&nested);

    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(stderr, pending_on_feature(&trees));
}

/// Under `tracked`, a linked worktree inside the main worktree on a branch
/// that holds no graph has none: it neither reads nor writes the main
/// worktree's.
#[test]
fn a_nested_worktree_of_a_tracked_graph_never_reaches_the_main_one() {
    let trees = Trees::tracked();
    let nested = trees.add_nested("HEAD~1");
    let before = trees.graph().join("decisions");
    let decisions = || fs::read_dir(&before).unwrap().count();
    let held = decisions();

    for start in [nested.clone(), nested.join("src")] {
        let stderr = trurlic_refused(&start, &["status"]);
        assert!(stderr.contains("not a trurlic project"), "{stderr}");
        let batch = nested.join("batch.json");
        let ops = json!({ "origin": "nested", "ops": [record("from-nested", "auth", "A rule")] });
        fs::write(&batch, ops.to_string()).unwrap();
        let stderr = trurlic_refused(&start, &["apply", batch.to_str().unwrap()]);
        assert!(stderr.contains("not a trurlic project"), "{stderr}");
        let (mut client, _) = McpClient::connect_in(&start, &[], json!({}));
        let refusal = client.call_tool_refusal("apply", ops);
        assert_eq!(refusal["kind"], "no_project", "{refusal}");
    }
    assert_eq!(decisions(), held);
}

/// A repository cloned inside a project's tree is a tree of its own: the
/// project's graph above it is not its graph.
#[test]
fn a_repository_inside_a_project_does_not_reach_its_graph() {
    let trees = Trees::tracked();
    let inner = trees.main.join("vendor/lib");
    fs::create_dir_all(inner.join("src")).unwrap();
    git(&inner, &["init", "--quiet", "--initial-branch=main"]);

    for start in [inner.clone(), inner.join("src")] {
        let stderr = trurlic_refused(&start, &["status"]);
        assert!(stderr.contains("not a trurlic project"), "{stderr}");
    }
    trurlic_ok(&trees.main.join("vendor"), &["status"]);
}

// -- check refuses what git contradicts -------------------------------------

#[test]
fn check_refuses_a_tracked_graph_git_tracks_none_of() {
    let project = Project::init_uncommitted();

    let stderr = project.run_refused(&["check"]);

    assert!(
        stderr.contains("placement = \"tracked\", but git tracks none of its 1 files"),
        "{stderr}"
    );
    git(project.path(), &["add", ".trurlic"]);
    project.run_ok(&["check"]);
}

#[test]
fn check_refuses_a_tracked_graph_git_ignores_a_file_of() {
    let project = Project::init();
    fs::write(project.path().join(".gitignore"), ".trurlic/decisions/\n").unwrap();
    project.write(
        "decisions/sign-tokens.toml",
        &decision("project", "Sign tokens", ""),
    );

    let stderr = project.run_refused(&["check"]);

    assert!(
        stderr.contains("git ignores 1 of its 2 files: decisions/sign-tokens.toml;"),
        "{stderr}"
    );
}

#[test]
fn check_refuses_a_tracked_graph_in_no_work_tree() {
    let project = Project::shared();
    let text = project.read("project.toml");
    project.write("project.toml", &text.replace("\"shared\"", "\"tracked\""));

    let stderr = project.run_refused(&["check"]);

    assert!(
        stderr.contains("placement = \"tracked\", but no git work tree holds it"),
        "{stderr}"
    );
}

/// The same graph, the same issue, from the main and the linked worktree.
/// `git add` passes over a file of the graph's own repository, so the
/// project's index takes it through `update-index`.
#[test]
fn check_refuses_a_shared_graph_git_tracks_from_every_tree() {
    let trees = Trees::shared();
    let object = git(&trees.main, &["hash-object", "-w", ".trurlic/project.toml"]);
    let object = String::from_utf8(object.stdout).unwrap();
    git(
        &trees.main,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("100644,{},.trurlic/project.toml", object.trim()),
        ],
    );

    for tree in [&trees.main, &trees.linked] {
        let stderr = trurlic_refused(tree, &["check"]);
        assert!(
            stderr.contains(&format!(
                "{}: placement = \"shared\", but git tracks 1 of its 3 files: project.toml;",
                trees.graph().display()
            )),
            "{stderr}"
        );
    }
}

/// A linked worktree that carries its own `.trurlic/` hides the shared
/// graph from itself: `check` in the main worktree names it with its
/// branch, and `check` in it names it beside the main worktree's graph.
#[test]
fn check_refuses_a_second_graph_beside_a_shared_one_from_both_sides() {
    let trees = Trees::shared();
    let second = trees.linked.join(".trurlic");
    fs::create_dir_all(second.join("components")).unwrap();
    fs::create_dir_all(second.join("decisions")).unwrap();
    fs::copy(
        trees.graph().join("project.toml"),
        second.join("project.toml"),
    )
    .unwrap();

    let stderr = trurlic_refused(&trees.main, &["check"]);
    assert!(
        stderr.contains(&format!(
            "{}: the worktree {} (branch feature) carries a graph of its own beside the \
             shared graph {}",
            second.display(),
            trees.linked.display(),
            trees.graph().display()
        )),
        "{stderr}"
    );
    let stderr = trurlic_refused(&trees.linked, &["check"]);
    assert!(
        stderr.contains(&format!(
            "{}: a second graph beside the shared graph {} of the main worktree",
            second.display(),
            trees.graph().display()
        )),
        "{stderr}"
    );
    // The walk finds the second graph first: the linked worktree reads it.
    let status = trurlic_ok(&trees.linked, &["status"]);
    assert!(status.contains("components: 0\n"), "{status}");
}

/// A shared graph below the top of the main worktree is out of reach of
/// the linked worktrees' lookup.
#[test]
fn check_refuses_a_shared_graph_off_the_top_of_the_main_worktree() {
    let trees = Trees::repository();
    let made = Project::shared();
    let sub = trees.main.join("sub");
    fs::create_dir_all(&sub).unwrap();
    fs::rename(made.graph(), sub.join(".trurlic")).unwrap();

    let stderr = trurlic_refused(&sub, &["check"]);

    assert!(
        stderr.contains(&format!(
            "placement = \"shared\", but linked worktrees find a shared graph only at the top \
             of the main worktree, {}",
            trees.main.display()
        )),
        "{stderr}"
    );
}

// -- init refuses a placement git cannot hold -------------------------------

#[test]
fn init_tracked_needs_a_work_tree_whose_git_does_not_ignore_the_graph() {
    let plain = TempDir::new().unwrap();
    let stderr = trurlic_refused(plain.path(), &["init", "--placement", "tracked"]);
    assert!(
        stderr.contains("placement tracked needs the project's git, and no git work tree holds"),
        "{stderr}"
    );

    let ignoring = TempDir::new().unwrap();
    git(ignoring.path(), &["init", "--quiet"]);
    fs::write(ignoring.path().join(".gitignore"), ".trurlic/\n").unwrap();
    let stderr = trurlic_refused(ignoring.path(), &["init", "--placement", "tracked"]);
    assert!(
        stderr.contains("so it cannot version a tracked graph"),
        "{stderr}"
    );

    for dir in [plain.path(), ignoring.path()] {
        assert!(!dir.join(".trurlic").exists());
    }
}

/// Only the top of the main worktree takes a shared graph; once it has
/// one, no linked worktree takes a graph at all.
#[test]
fn init_shared_belongs_at_the_top_of_the_main_worktree() {
    let trees = Trees::repository();
    trees.add_linked();
    for dir in [trees.linked.clone(), trees.main.join("src")] {
        let stderr = trurlic_refused(&dir, &["init", "--placement", "shared"]);
        assert!(
            stderr.contains(&format!(
                "a shared graph lives at the top of the main worktree, {}",
                trees.main.display()
            )),
            "{stderr}"
        );
    }

    let stdout = trurlic_ok(&trees.main, &["init", "--placement", "shared"]);

    let exclude = trees.main.join(".git/info/exclude");
    assert_eq!(
        stdout,
        format!(
            "Initialized .trurlic/ (shared), its history in .trurlic/.git: kept out of the \
             project's git by {}\n",
            exclude.display()
        )
    );
    let excluded = fs::read_to_string(&exclude).unwrap();
    assert!(
        excluded.lines().any(|line| line == "/.trurlic/"),
        "{excluded}"
    );
    let status = git(
        &trees.main,
        &["status", "--porcelain", "--untracked-files=all"],
    );
    assert_eq!(String::from_utf8(status.stdout).unwrap(), "");
    assert!(!trees.main.join(".gitignore").exists());
    for placement in ["tracked", "shared"] {
        let stderr = trurlic_refused(&trees.linked, &["init", "--placement", placement]);
        assert!(
            stderr.contains(&format!(
                "the shared graph {} of the main worktree serves this worktree",
                trees.graph().display()
            )),
            "{stderr}"
        );
    }
}

#[test]
fn init_shared_in_a_worktree_of_a_bare_repository_is_refused() {
    let trees = Trees::repository();
    git(
        &trees.root,
        &["clone", "--quiet", "--bare", "main", "bare.git"],
    );
    let bare = trees.root.join("bare.git");
    git(&bare, &["worktree", "add", "--quiet", "../work"]);

    let stderr = trurlic_refused(&trees.root.join("work"), &["init", "--placement", "shared"]);

    assert!(
        stderr.contains("is bare: it has no main worktree"),
        "{stderr}"
    );
    // A bare repository holds no graph and finds none.
    let stderr = trurlic_refused(&bare, &["status"]);
    assert!(stderr.contains("not a trurlic project"), "{stderr}");
}

// -- hostile layouts -------------------------------------------------------

/// A dangling `.trurlic` symlink and one that loops are typed errors in
/// the CLI and tool refusals in the server, never a hang.
#[test]
fn a_broken_symlink_is_refused_by_every_surface() {
    let tmp = TempDir::new().unwrap();
    for (name, target) in [("dangling", "../gone"), ("looping", ".trurlic")] {
        let dir = tmp.path().join(name);
        fs::create_dir_all(&dir).unwrap();
        symlink(Path::new(target), &dir.join(".trurlic"));

        let stderr = trurlic_refused(&dir, &["status"]);
        assert!(
            stderr.contains("the symlink to the graph leads nowhere"),
            "{name}: {stderr}"
        );
        let (mut client, _) = McpClient::connect_in(&dir, &[], json!({}));
        let refusal = client.call_tool_refusal("get_context", json!({ "component": "auth" }));
        assert_eq!(refusal["kind"], "store_error", "{name}: {refusal}");
    }
}

/// The linked worktree of a shared graph whose main worktree is gone: git
/// refuses the worktree, and the refusal is git's, typed. Under `tracked`
/// the worktree still reads its own graph.
#[test]
fn a_worktree_whose_main_worktree_is_gone_says_what_git_says() {
    let shared = Trees::shared();
    fs::remove_dir_all(&shared.main).unwrap();
    let stderr = trurlic_refused(&shared.linked, &["status"]);
    assert!(stderr.contains("`git worktree list"), "{stderr}");

    let tracked = Trees::tracked();
    fs::remove_dir_all(&tracked.main).unwrap();
    let status = trurlic_ok(&tracked.linked, &["status"]);
    assert!(status.contains("components: 1\n"), "{status}");
}

/// With no `git` on `PATH`, a tree that holds its graph, and a directory
/// inside the graph, are served: discovery spawned no git. The linked
/// worktree needs git to reach the shared graph and says so, and so does
/// `status`, which asks the graph's repository for what it lacks.
#[test]
fn discovery_runs_no_git_where_the_walk_finds_the_graph() {
    let trees = Trees::shared();
    let empty = TempDir::new().unwrap();
    let no_git = |dir: &Path, args: &[&str]| {
        trurlic(dir)
            .args(args)
            .env("PATH", empty.path())
            .output()
            .unwrap()
    };

    for dir in [trees.main.join("src"), trees.graph().join("decisions")] {
        let output = no_git(&dir, &["query", "file", "src/branch.rs"]);
        assert!(output.status.success(), "{}: {output:?}", dir.display());
        let (mut client, _) = McpClient::connect_in(&dir, &[("PATH", "")], json!({}));
        client.call_tool("get_context", json!({ "component": "auth" }));
    }
    let output = no_git(&trees.main, &["status"]);
    assert_eq!(output.status.code(), Some(1));
    let output = no_git(&trees.linked, &["status"]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("cannot run `git`"), "{stderr}");
}

// -- one history for every tree of one graph --------------------------------

/// A write from the main worktree, the linked worktree and the symlinked
/// clone of one shared graph each commits to the graph's own repository,
/// after the hand edit it found; the project's repository and the clone's
/// get no commit.
#[test]
fn every_tree_of_a_shared_graph_commits_to_its_one_repository() {
    let trees = Trees::shared();
    let head = |tree: &Path| String::from_utf8(git(tree, &["rev-parse", "HEAD"]).stdout).unwrap();
    let heads = [head(&trees.main), head(&trees.linked), head(&trees.clone)];
    let before = graph_log(&trees.graph()).len();

    for tree in [&trees.main, &trees.linked, &trees.clone] {
        fs::write(
            trees.graph().join("components/auth.toml"),
            format!("description='{}'\n", tree.display()),
        )
        .unwrap();
        trurlic_ok(tree, &["fmt"]);
    }

    let log = graph_log(&trees.graph());
    assert_eq!(log.len(), before + 6, "{log:?}");
    assert_eq!(
        log[..6],
        ["fmt", "hand edit", "fmt", "hand edit", "fmt", "hand edit"]
    );
    assert_eq!(
        [head(&trees.main), head(&trees.linked), head(&trees.clone)],
        heads
    );
}

/// Three servers, one in each tree of one shared graph, apply batches at
/// once. Every batch lands, each server's next read sees the others'
/// writes, and the graph's repository holds exactly one commit per batch,
/// with its origin.
#[test]
fn concurrent_batches_from_every_tree_each_land_as_one_commit() {
    const BATCHES: usize = 6;
    let trees = Trees::shared();
    let before = graph_log(&trees.graph());
    let writers: Vec<_> = [
        ("main", &trees.main),
        ("linked", &trees.linked),
        ("clone", &trees.clone),
    ]
    .into_iter()
    .map(|(tree, dir)| {
        let dir = dir.clone();
        std::thread::spawn(move || {
            let (mut client, _) = McpClient::connect_in(&dir, &[], json!({}));
            for batch in 0..BATCHES {
                let name = format!("{tree}-{batch}");
                client.call_tool(
                    "apply",
                    json!({ "origin": name, "ops": [
                        record(&name, "auth", &format!("Rule {name}")),
                    ]}),
                );
            }
            client
        })
    })
    .collect();
    let mut clients: Vec<McpClient> = writers
        .into_iter()
        .map(|writer| writer.join().unwrap())
        .collect();

    for client in &mut clients {
        let status = client.call_tool("status", json!({}));
        assert_eq!(status["counts"]["decisions"], 1 + 3 * BATCHES, "{status}");
    }
    let log = graph_log(&trees.graph());
    assert_eq!(log.len(), before.len() + 3 * BATCHES, "{log:?}");
    let origins: std::collections::BTreeSet<&str> = log[..3 * BATCHES]
        .iter()
        .map(|message| message.strip_prefix("apply\n\nOrigin: ").unwrap())
        .collect();
    assert_eq!(origins.len(), 3 * BATCHES, "{log:?}");
    assert_eq!(log[3 * BATCHES..], before[..]);
    trurlic_ok(&trees.linked, &["check"]);
}

// -- one lock for every tree of one graph -----------------------------------

/// A hand edit `trurlic fmt` rewrites.
#[cfg(feature = "failpoints")]
fn non_canonical(trees: &Trees) {
    fs::write(
        trees.graph().join("components/auth.toml"),
        "description='Auth'\n",
    )
    .unwrap();
}

/// A writer in the main worktree holds the lock with its files staged.
/// Writers started in the linked worktree and in the clone wait for it,
/// then each loads the graph it committed and finds nothing to do.
#[cfg(feature = "failpoints")]
#[test]
fn writers_in_every_tree_of_one_graph_take_one_lock() {
    use std::process::Stdio;
    use std::time::Duration;

    use crate::harness::Pause;

    let trees = Trees::shared();
    non_canonical(&trees);
    let pause = Pause::arm_in(&trees.root, "commit.staged:1");
    let holder = trurlic(&trees.main)
        .arg("fmt")
        .envs(pause.env())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    pause.wait();
    let mut waiting: Vec<_> = [&trees.linked, &trees.clone]
        .into_iter()
        .map(|tree| {
            trurlic(tree)
                .arg("fmt")
                .stdout(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();

    // Several lock polls, well inside the lock timeout.
    std::thread::sleep(Duration::from_millis(500));
    for writer in &mut waiting {
        assert!(
            writer.try_wait().unwrap().is_none(),
            "a writer did not wait"
        );
    }
    pause.resume();

    let first = holder.wait_with_output().unwrap();
    assert!(first.status.success(), "{first:?}");
    assert_eq!(
        String::from_utf8(first.stdout).unwrap(),
        "formatted components/auth.toml\n"
    );
    for writer in waiting {
        let output = writer.wait_with_output().unwrap();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!("{}/ is in canonical form\n", trees.graph().display())
        );
    }
}

/// A writer in the linked worktree stops after its journal, the commit
/// point. The journal is in the graph's `.state/`, and a read in the clone
/// finishes the commit.
#[cfg(feature = "failpoints")]
#[test]
fn a_commit_stopped_in_one_tree_is_finished_from_another() {
    let trees = Trees::shared();
    non_canonical(&trees);
    let journal = trees.graph().join(".state/txn.toml");

    let stopped = trurlic(&trees.linked)
        .arg("fmt")
        .env("TRURLIC_FAILPOINT", "commit.journaled:1")
        .output()
        .unwrap();
    assert!(!stopped.status.success(), "{stopped:?}");
    assert!(journal.is_file());

    trurlic_ok(&trees.clone, &["status"]);

    assert!(!journal.exists());
    assert_eq!(
        fs::read_to_string(trees.graph().join("components/auth.toml")).unwrap(),
        "description = \"Auth\"\n"
    );
}
