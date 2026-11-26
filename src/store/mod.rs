//! The decision graph on disk: `.trurlic/` read into a [`ProjectState`],
//! validated, and written through a journaled commit, a [`Writer`]'s for
//! every write but `init` and `migrate`, which under `shared` placement ends with a commit to the graph's own
//! git repository (`history.rs`).

mod access;
mod apply;
mod canonical;
mod commit;
#[cfg(any(test, feature = "bench"))]
pub(crate) mod corpus;
mod create;
mod cycles;
mod durable;
pub(crate) mod failpoint;
mod history;
mod journal;
mod load;
mod lock;
mod migration;
mod model;
mod placement;
mod quality;
mod query;
mod recovery;
#[cfg(any(test, feature = "bench"))]
pub(crate) mod seeded;
mod symbols;
mod validate;
mod version;

use std::fmt;
use std::path::{self, Path, PathBuf};

use crate::{Error, Result};

pub use access::Writer;
pub use apply::{
    AddComponent, AnchorInput, Applied, Batch, Done, Field, LeftRepair, ListEdit, Merge, Op,
    OpKind, PatternNote, Record, Refusal, Remove, RemoveComponent, Rename, Repair, RepairAnchors,
    Repaired, Revise, ReviseComponent, Warning, WarningKind,
};
pub(crate) use durable::sync_dir;
pub use history::HistoryFinding;
pub use lock::StoreLock;
pub use migration::PlacementDoubt;
pub(crate) use migration::{
    Fact, Loss, MigrationReport, PatternRecord, PlacementBasis, PlacementSource,
};
pub(crate) use model::limits;
pub use model::{
    Anchor, AnchorFault, COMPONENTS_DIR, Component, DECISIONS_DIR, Decision, FORMAT_VERSION,
    FormatVersion, Level, NodeFile, PROJECT_COMPONENT, PROJECT_FILE, Placement, ProjectState,
    Quality, STATE_DIR, STORE_DIR, Status, StatusName, TextFault, TextField, Timestamp,
    TimestampError,
};
pub use placement::{LinkedGraph, MainWorktree, PlacementFacts, WorkTreeFacts};
pub(crate) use quality::Bar;
pub use quality::{PreparedBar, QualityFault, Shortfall};
pub use query::{Named, anchors_in, normalize_path};
pub use symbols::SymbolsKey;
pub use validate::{Issue, IssueKind};

#[cfg(test)]
pub(crate) use apply::{NodeKind, OpAt, Rule};
#[cfg(test)]
pub(crate) use model::{AcceptedText, DecisionField, NameFault, Pattern, Project, WordRange};
#[cfg(test)]
pub(crate) use quality::PatternMatch;

/// Handle to a graph directory and the code root it is read for. It holds
/// no graph: a read loads one with [`Store::load_state`], and every write
/// to a current-format graph goes through the [`Writer`] that
/// [`Store::begin_write`] returns.
///
/// The two roots differ when a tree reaches a graph it does not hold: a
/// symlinked clone, a linked worktree of a shared graph. Anchors resolve
/// against the code root, and `.state/`, the lock and the journal, lives in
/// the graph directory, so every tree that reaches one graph shares one
/// lock.
#[derive(Debug)]
pub struct Store {
    root: PathBuf,
    code_root: PathBuf,
}

impl Store {
    /// The graph at `root`, read for the directory that holds it.
    pub fn at(root: PathBuf) -> Self {
        let code_root = root.parent().unwrap_or(&root).to_path_buf();
        Self { root, code_root }
    }

    /// The graph at `root`, read for the tree at `code_root`.
    pub fn reached_from(root: PathBuf, code_root: PathBuf) -> Self {
        Self { root, code_root }
    }

    /// The graph directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The tree the graph is read for, which anchors are relative to.
    pub fn code_root(&self) -> &Path {
        &self.code_root
    }

    /// Whether the graph directory is the code root's own `.trurlic/`, as
    /// opposed to one reached through a symlink or a linked worktree.
    pub fn is_local(&self) -> bool {
        self.root == self.code_root.join(STORE_DIR)
    }

    fn state_dir(&self) -> PathBuf {
        self.root.join(STATE_DIR)
    }

    fn tmp_dir(&self) -> PathBuf {
        self.state_dir().join("tmp")
    }

    fn lock_path(&self) -> PathBuf {
        self.state_dir().join("lock")
    }

    pub fn component_path(&self, name: &str) -> PathBuf {
        self.root.join(COMPONENTS_DIR).join(format!("{name}.toml"))
    }

    pub fn decision_path(&self, name: &str) -> PathBuf {
        self.root.join(DECISIONS_DIR).join(format!("{name}.toml"))
    }

    fn path_of(&self, file: &NodeFile) -> PathBuf {
        self.root.join(file.to_string())
    }

    /// The directory that holds the graph directory, and the graph
    /// directory's name in it.
    pub(crate) fn parent_and_name(&self) -> Result<(&Path, &Path)> {
        match (self.root.parent(), self.root.file_name()) {
            (Some(parent), Some(name)) => Ok((parent, Path::new(name))),
            _ => Err(Error::StorePath {
                // clone: the error owns the path it names.
                path: self.root.clone(),
                fault: StorePathFault::NoParent,
            }),
        }
    }

    /// Refuse `path` unless it is below the graph directory with no `..`
    /// in it. Every path a write touches is derived from the root, so this
    /// fires only on a programming error that would write or delete a file
    /// outside `.trurlic/`. `..` is refused before the prefix test, which
    /// compares components and would pass `<root>/../elsewhere`.
    fn verify_path(&self, path: &Path) -> Result<()> {
        let fault = if path.components().any(|c| c == path::Component::ParentDir) {
            StorePathFault::Traversal
        } else if path.starts_with(&self.root) {
            return Ok(());
        } else {
            StorePathFault::Outside
        };
        Err(Error::StorePath {
            path: path.to_path_buf(),
            fault,
        })
    }
}

/// How a path the store derived breaks its layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorePathFault {
    /// It holds a `..` component.
    Traversal,
    /// It is not below the graph directory.
    Outside,
    NoParent,
}

impl fmt::Display for StorePathFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Traversal => "holds a `..` component",
            Self::Outside => "is outside the graph directory",
            Self::NoParent => "has no parent directory",
        })
    }
}

// -- Test helpers ---------------------------------------------------------------

#[cfg(test)]
pub(crate) mod testing {
    use std::collections::BTreeMap;
    use std::fs;

    use super::canonical::{Canonical, canonical};
    use super::*;

    pub fn setup_store(dir: &Path) -> Store {
        setup_store_with_version(dir, &FORMAT_VERSION.to_string())
    }

    /// An empty store whose `project.toml` states `version` as its
    /// `format_version`, whatever the text, beside the fields of format 0.5.
    pub fn setup_store_with_version(dir: &Path, version: &str) -> Store {
        let root = dir.join(STORE_DIR);
        fs::create_dir_all(root.join(COMPONENTS_DIR)).unwrap();
        fs::create_dir_all(root.join(DECISIONS_DIR)).unwrap();
        fs::create_dir_all(root.join(STATE_DIR)).unwrap();
        let project = format!(
            "format_version = \"{version}\"\nname = \"test-project\"\n\
             description = \"A test project\"\nplacement = \"tracked\"\n"
        );
        fs::write(root.join(PROJECT_FILE), project).unwrap();
        Store::at(root)
    }

    /// An empty `shared` store, whose repository holds the `init` commit.
    pub fn shared_store(dir: &Path) -> Store {
        Store::create(dir, "test-project", Placement::Shared).unwrap()
    }

    /// `git <args>` in the graph directory of `store`, as a person with no
    /// user or system configuration; it must succeed.
    pub fn graph_git(store: &Store, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(store.root())
            .args([
                "-c",
                "user.name=someone",
                "-c",
                "user.email=someone@example.invalid",
            ])
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap()
    }

    /// The messages of the graph's history, newest first, each trimmed of
    /// the line breaks around it.
    pub fn graph_log(store: &Store) -> Vec<String> {
        graph_git(store, &["log", "--format=%B%x00"])
            .split('\0')
            .map(|message| message.trim_matches('\n').to_owned())
            .filter(|message| !message.is_empty())
            .collect()
    }

    /// The files the commit `rev` changed, sorted.
    pub fn changed_in(store: &Store, rev: &str) -> Vec<String> {
        let names = graph_git(
            store,
            &["show", "--name-only", "--format=", "--no-renames", rev],
        );
        let mut names: Vec<String> = names.lines().map(str::to_owned).collect();
        names.sort();
        names
    }

    /// `value` written to `file` in its canonical form, outside any commit.
    pub fn write_node<T: Canonical>(store: &Store, file: &NodeFile, value: &T) {
        fs::write(store.path_of(file), canonical(value)).unwrap();
    }

    pub fn ts() -> Timestamp {
        "2025-06-01T10:00:00Z".parse().unwrap()
    }

    pub fn sample_component() -> Component {
        Component {
            description: "A sample component".into(),
            connects_to: std::collections::BTreeSet::new(),
        }
    }

    /// An accepted decision of `component` with no sets and no origin.
    pub fn sample_decision(component: &str) -> Decision {
        Decision {
            component: component.into(),
            choice: "Choice of the sample".into(),
            reason: "Reason of the sample decision".into(),
            alternatives: vec![],
            tags: std::collections::BTreeSet::new(),
            status: Status::Accepted { via: None },
            origin: None,
            created: ts(),
            anchors: std::collections::BTreeSet::new(),
            depends_on: std::collections::BTreeSet::new(),
        }
    }

    /// A `[quality]` pattern no cache holds over [`coin_flips`]: its lazy
    /// DFA tracks where the last twenty-one letters hold an `a`.
    pub const OUTGROWS_ITS_CACHE: &str = "(?:a|b)*a(?:a|b){20}$";

    /// `len` letters `a` and `b` in an order fixed by `seed`, with no
    /// pattern to them a short window repeats.
    pub fn coin_flips(len: usize, seed: u64) -> String {
        let mut state = seed;
        (0..len)
            .map(|_| {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                if state >> 63 == 0 { 'a' } else { 'b' }
            })
            .collect()
    }

    /// [`sample_decision`] anchored at each of `files`.
    pub fn anchored(component: &str, files: &[&str]) -> Decision {
        let mut decision = sample_decision(component);
        decision.anchors = files
            .iter()
            .map(|&file| Anchor::new(file.into(), None).unwrap())
            .collect();
        decision
    }

    /// A state with no node.
    pub fn empty_state() -> ProjectState {
        ProjectState {
            project: Project {
                format_version: FORMAT_VERSION,
                name: "test".into(),
                description: String::new(),
                placement: Placement::Tracked,
                quality: Quality::default(),
            },
            components: BTreeMap::new(),
            decisions: BTreeMap::new(),
            non_canonical: Vec::new(),
            code_root: PathBuf::new(),
        }
    }

    /// Project-wide decisions joined by `depends_on` edges in the given
    /// order: a graph whose only issues are the ones the edges make.
    pub fn depends_on_state(edges: &[(&str, &str)]) -> ProjectState {
        let mut state = empty_state();
        for &(from, to) in edges {
            for name in [from, to] {
                state
                    .decisions
                    .entry(name.to_owned())
                    .or_insert_with(|| sample_decision(PROJECT_COMPONENT));
            }
            if let Some(decision) = state.decisions.get_mut(from) {
                decision.depends_on.insert(to.to_owned());
            }
        }
        state
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn verify_path_accepts_a_child_and_refuses_an_escape() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        store.verify_path(&store.component_path("auth")).unwrap();

        let outside = tmp.path().join("outside.toml");
        assert!(matches!(
            store.verify_path(&outside),
            Err(Error::StorePath { fault: StorePathFault::Outside, path }) if path == outside
        ));
        let traversal = store.root().join("..").join("escape.toml");
        assert!(matches!(
            store.verify_path(&traversal),
            Err(Error::StorePath {
                fault: StorePathFault::Traversal,
                ..
            })
        ));
    }
}
