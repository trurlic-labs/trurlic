//! Whether git agrees with the placement a graph states. The caller runs
//! git and hands over its answers as [`PlacementFacts`]; the store judges
//! them here and spawns no process, as the migration judges the listing it
//! is given. Both read a listing by one rule, [`partition`]: a file of the
//! graph is in it when the listing holds its path under the graph
//! directory.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::validate::{Issue, IssueKind, issue, sort};
use crate::store::model::Placement;
use crate::store::model::ProjectState;

/// The names a list of files shows before it counts the rest.
const SHOWN_FILES: usize = 5;

/// What git says about the directory that holds a graph.
#[derive(Debug)]
pub enum PlacementFacts {
    /// No git work tree holds the graph directory.
    NoWorkTree,
    WorkTree(WorkTreeFacts),
}

/// The answers of the repository whose work tree holds a graph. Paths are
/// relative to the graph directory, and may name files that are not the
/// graph's, such as those under `.state/`.
#[derive(Debug)]
pub struct WorkTreeFacts {
    /// The files the index holds.
    pub tracked: BTreeSet<PathBuf>,
    /// The files the index does not hold and an ignore rule matches.
    pub ignored: BTreeSet<PathBuf>,
    pub main: MainWorktree,
}

/// The repository's main worktree, as it bears on a graph.
#[derive(Debug)]
pub enum MainWorktree {
    /// The repository is bare and has none.
    Bare,
    /// Its `.trurlic` leads to this graph; `linked` are the other graph
    /// directories the linked worktrees carry.
    Here { linked: Vec<LinkedGraph> },
    /// Its top is `top`, and its `.trurlic`, if any, is another graph;
    /// `shared` is that graph when it states `shared`.
    Elsewhere {
        top: PathBuf,
        shared: Option<PathBuf>,
    },
}

/// A graph directory a linked worktree carries.
#[derive(Debug)]
pub struct LinkedGraph {
    pub graph: PathBuf,
    pub worktree: PathBuf,
    /// The branch checked out there; `None` on a detached HEAD.
    pub branch: Option<String>,
}

impl ProjectState {
    /// Every way `facts` contradict the placement of this graph, read from
    /// the directory `graph`, sorted as [`validate`](Self::validate) sorts.
    #[must_use]
    pub fn placement_issues(&self, graph: &Path, facts: &PlacementFacts) -> Vec<Issue> {
        let at = graph.display().to_string();
        let mut issues = Vec::new();
        match (self.project.placement, facts) {
            (Placement::Tracked, PlacementFacts::NoWorkTree) => issues.push(issue(
                IssueKind::TrackedUnversioned,
                &at,
                format!(
                    "{at}: placement = \"tracked\", but no git work tree holds it; put the \
                     project under git, or state placement = \"shared\" in project.toml"
                ),
            )),
            (Placement::Shared, PlacementFacts::NoWorkTree) => {}
            (placement, PlacementFacts::WorkTree(facts)) => {
                let files = self.files();
                match placement {
                    Placement::Tracked => tracked_issues(&at, &files, facts, &mut issues),
                    Placement::Shared => shared_issues(&at, &files, facts, &mut issues),
                }
                second_graphs(&at, placement, &facts.main, &mut issues);
            }
        }
        sort(&mut issues);
        issues
    }
}

/// A tracked graph: git tracks a file of it, and ignores none. A file not
/// added yet, such as a new decision, is a change waiting for a commit.
fn tracked_issues(at: &str, files: &[String], facts: &WorkTreeFacts, issues: &mut Vec<Issue>) {
    let total = files.len();
    if partition(files.iter().map(String::as_str), &facts.tracked)
        .0
        .is_empty()
    {
        issues.push(issue(
            IssueKind::TrackedUnversioned,
            at,
            format!(
                "{at}: placement = \"tracked\", but git tracks none of its {total} files; add \
                 them with `git add`, or state placement = \"shared\" in project.toml"
            ),
        ));
    }
    let (ignored, _) = partition(files.iter().map(String::as_str), &facts.ignored);
    if !ignored.is_empty() {
        issues.push(issue(
            IssueKind::TrackedIgnored,
            at,
            format!(
                "{at}: placement = \"tracked\", but git ignores {} of its {total} files: {}; \
                 remove the rule that matches them",
                ignored.len(),
                listed(&ignored)
            ),
        ));
    }
}

/// A shared graph: git tracks no file of it, and it is the main
/// worktree's, where every linked worktree finds it.
fn shared_issues(at: &str, files: &[String], facts: &WorkTreeFacts, issues: &mut Vec<Issue>) {
    let (tracked, _) = partition(files.iter().map(String::as_str), &facts.tracked);
    if !tracked.is_empty() {
        issues.push(issue(
            IssueKind::SharedVersioned,
            at,
            format!(
                "{at}: placement = \"shared\", but git tracks {} of its {} files: {}; take them \
                 out of the index with `git rm -r --cached`, or state placement = \"tracked\" \
                 in project.toml",
                tracked.len(),
                files.len(),
                listed(&tracked)
            ),
        ));
    }
    let off_main = match &facts.main {
        MainWorktree::Bare => Some(
            "its repository is bare, so no main worktree holds a graph the linked worktrees find"
                .to_owned(),
        ),
        MainWorktree::Elsewhere { top, shared: None } => Some(format!(
            "linked worktrees find a shared graph only at the top of the main worktree, {}",
            top.display()
        )),
        // The main worktree's graph: this one, or a shared one that makes
        // this one a second graph.
        MainWorktree::Here { .. }
        | MainWorktree::Elsewhere {
            shared: Some(_), ..
        } => None,
    };
    if let Some(why) = off_main {
        issues.push(issue(
            IssueKind::SharedOffMain,
            at,
            format!("{at}: placement = \"shared\", but {why}"),
        ));
    }
}

/// Under a shared graph in the main worktree, every other graph directory
/// of its worktrees, whatever placement this graph states: each one hides
/// the shared graph from its worktree.
fn second_graphs(at: &str, placement: Placement, main: &MainWorktree, issues: &mut Vec<Issue>) {
    match main {
        MainWorktree::Here { linked } if placement == Placement::Shared => {
            for second in linked {
                let graph = second.graph.display().to_string();
                let head = second
                    .branch
                    .as_ref()
                    .map_or_else(|| "detached HEAD".to_owned(), |b| format!("branch {b}"));
                issues.push(issue(
                    IssueKind::SecondGraph,
                    &graph,
                    format!(
                        "{graph}: the worktree {} ({head}) carries a graph of its own beside \
                         the shared graph {at} that serves it; remove it",
                        second.worktree.display()
                    ),
                ));
            }
        }
        MainWorktree::Elsewhere {
            shared: Some(main), ..
        } => issues.push(issue(
            IssueKind::SecondGraph,
            at,
            format!(
                "{at}: a second graph beside the shared graph {} of the main worktree, which \
                 serves this worktree; remove it",
                main.display()
            ),
        )),
        // A tracked graph in the main worktree, whose linked worktrees
        // carry their own; a bare repository; no shared graph.
        MainWorktree::Here { .. } | MainWorktree::Bare | MainWorktree::Elsewhere { .. } => {}
    }
}

/// `files` split into those `listing` holds and the rest, each in order;
/// borrowed or owned as `files` yields them.
pub(super) fn partition<S: AsRef<str>>(
    files: impl IntoIterator<Item = S>,
    listing: &BTreeSet<PathBuf>,
) -> (Vec<S>, Vec<S>) {
    files
        .into_iter()
        .partition(|file| listing.contains(Path::new(file.as_ref())))
}

/// `files` comma-separated, the first [`SHOWN_FILES`] named and the rest
/// counted.
pub(super) fn listed<S: AsRef<str>>(files: &[S]) -> String {
    let shown: Vec<&str> = files.iter().take(SHOWN_FILES).map(AsRef::as_ref).collect();
    match files.len().saturating_sub(SHOWN_FILES) {
        0 => shown.join(", "),
        more => format!("{} and {more} more", shown.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testing::{empty_state, sample_component, sample_decision};

    const GRAPH: &str = "/work/main/.trurlic";

    /// A graph of `placement` with one component and one decision: three
    /// files.
    fn state(placement: Placement) -> ProjectState {
        let mut state = empty_state();
        state.project.placement = placement;
        state.components.insert("auth".into(), sample_component());
        state.decisions.insert("d".into(), sample_decision("auth"));
        state
    }

    fn paths(files: &[&str]) -> BTreeSet<PathBuf> {
        files.iter().map(PathBuf::from).collect()
    }

    fn in_main(tracked: &[&str], ignored: &[&str]) -> PlacementFacts {
        PlacementFacts::WorkTree(WorkTreeFacts {
            tracked: paths(tracked),
            ignored: paths(ignored),
            main: MainWorktree::Here { linked: vec![] },
        })
    }

    fn kinds(issues: &[Issue]) -> Vec<(IssueKind, &str)> {
        issues
            .iter()
            .map(|issue| (issue.kind, issue.subject.as_str()))
            .collect()
    }

    fn judged(placement: Placement, facts: &PlacementFacts) -> Vec<Issue> {
        state(placement).placement_issues(Path::new(GRAPH), facts)
    }

    #[test]
    fn a_tracked_graph_needs_a_work_tree_and_one_tracked_file() {
        assert_eq!(
            kinds(&judged(Placement::Tracked, &PlacementFacts::NoWorkTree)),
            [(IssueKind::TrackedUnversioned, GRAPH)]
        );
        let none = judged(Placement::Tracked, &in_main(&[], &[]));
        assert_eq!(kinds(&none), [(IssueKind::TrackedUnversioned, GRAPH)]);
        assert!(none[0].message.contains("none of its 3 files"), "{none:?}");

        // A new decision not added yet is a change waiting for a commit.
        assert_eq!(
            judged(Placement::Tracked, &in_main(&["project.toml"], &[])),
            []
        );
    }

    #[test]
    fn a_tracked_graph_with_an_ignored_file_is_named_with_the_file() {
        let issues = judged(
            Placement::Tracked,
            &in_main(&["project.toml"], &["decisions/d.toml", ".state/lock"]),
        );

        assert_eq!(kinds(&issues), [(IssueKind::TrackedIgnored, GRAPH)]);
        assert!(
            issues[0]
                .message
                .contains("git ignores 1 of its 3 files: decisions/d.toml;"),
            "{issues:?}"
        );
    }

    #[test]
    fn a_shared_graph_git_tracks_is_named_with_the_files() {
        assert_eq!(judged(Placement::Shared, &PlacementFacts::NoWorkTree), []);
        assert_eq!(
            judged(Placement::Shared, &in_main(&[], &[".state/lock"])),
            []
        );

        let issues = judged(
            Placement::Shared,
            &in_main(&["components/auth.toml", "project.toml"], &[]),
        );

        assert_eq!(kinds(&issues), [(IssueKind::SharedVersioned, GRAPH)]);
        assert!(
            issues[0]
                .message
                .contains("git tracks 2 of its 3 files: project.toml, components/auth.toml;"),
            "{issues:?}"
        );
    }

    /// Facts that hold nothing against `placement` but what `main` says.
    fn with_main(placement: Placement, main: MainWorktree) -> PlacementFacts {
        let tracked = match placement {
            Placement::Tracked => paths(&["project.toml"]),
            Placement::Shared => BTreeSet::new(),
        };
        PlacementFacts::WorkTree(WorkTreeFacts {
            tracked,
            ignored: BTreeSet::new(),
            main,
        })
    }

    #[test]
    fn a_shared_graph_off_the_main_worktree_names_where_it_belongs() {
        let elsewhere = with_main(
            Placement::Shared,
            MainWorktree::Elsewhere {
                top: "/work/main".into(),
                shared: None,
            },
        );
        let issues = judged(Placement::Shared, &elsewhere);
        assert_eq!(kinds(&issues), [(IssueKind::SharedOffMain, GRAPH)]);
        assert!(
            issues[0].message.contains("main worktree, /work/main"),
            "{issues:?}"
        );

        let bare = judged(
            Placement::Shared,
            &with_main(Placement::Shared, MainWorktree::Bare),
        );
        assert_eq!(kinds(&bare), [(IssueKind::SharedOffMain, GRAPH)]);
        assert!(bare[0].message.contains("bare"), "{bare:?}");

        // A tracked graph may sit anywhere in a worktree.
        assert_eq!(
            judged(
                Placement::Tracked,
                &with_main(Placement::Tracked, MainWorktree::Bare)
            ),
            []
        );
    }

    #[test]
    fn every_graph_beside_a_shared_main_graph_is_a_second_graph() {
        let linked = |name: &str, branch: Option<&str>| LinkedGraph {
            graph: format!("/work/{name}/.trurlic").into(),
            worktree: format!("/work/{name}").into(),
            branch: branch.map(str::to_owned),
        };
        let here = |placement| {
            with_main(
                placement,
                MainWorktree::Here {
                    linked: vec![linked("b", None), linked("a", Some("feature"))],
                },
            )
        };

        let issues = judged(Placement::Shared, &here(Placement::Shared));

        assert_eq!(
            kinds(&issues),
            [
                (IssueKind::SecondGraph, "/work/a/.trurlic"),
                (IssueKind::SecondGraph, "/work/b/.trurlic"),
            ]
        );
        assert!(issues[0].message.contains("(branch feature)"), "{issues:?}");
        assert!(issues[1].message.contains("(detached HEAD)"), "{issues:?}");
        // Under a tracked main graph every worktree carries its own.
        assert_eq!(
            judged(Placement::Tracked, &here(Placement::Tracked)),
            [] as [Issue; 0]
        );

        for placement in [Placement::Tracked, Placement::Shared] {
            let beside = with_main(
                placement,
                MainWorktree::Elsewhere {
                    top: "/work/main".into(),
                    shared: Some("/work/main/.trurlic".into()),
                },
            );
            let found = state(placement).placement_issues(Path::new("/work/b/.trurlic"), &beside);
            assert_eq!(
                kinds(&found),
                [(IssueKind::SecondGraph, "/work/b/.trurlic")],
                "{placement:?}"
            );
        }
    }

    #[test]
    fn a_long_list_names_five_files_and_counts_the_rest() {
        let files: Vec<String> = (1..=7).map(|n| format!("decisions/d{n}.toml")).collect();

        assert_eq!(
            listed(&files),
            "decisions/d1.toml, decisions/d2.toml, decisions/d3.toml, decisions/d4.toml, \
             decisions/d5.toml and 2 more"
        );
        assert_eq!(listed(&files[..1]), "decisions/d1.toml");
    }
}
