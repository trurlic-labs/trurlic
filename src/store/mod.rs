//! The decision graph on disk: `.trurlic/` read into a [`ProjectState`],
//! validated, and written through the journaled commit of a [`Writer`].

mod access;
mod canonical;
mod commit;
#[cfg(any(test, feature = "bench"))]
pub mod corpus;
mod create;
mod cycles;
mod durable;
pub(crate) mod failpoint;
mod journal;
pub(crate) mod limits;
mod load;
mod lock;
mod migration;
mod placement;
mod query;
mod recovery;
pub mod schema;
mod state;
pub(crate) mod text;
mod time;
mod validate;
mod version;

use std::path::{Path, PathBuf};

use crate::{Error, Result};

pub use access::Writer;
pub(crate) use durable::sync_dir;
pub use lock::StoreLock;
pub use migration::PlacementDoubt;
pub(crate) use migration::{
    Fact, Loss, MigrationReport, PatternRecord, PlacementBasis, PlacementSource,
};
pub use placement::{LinkedGraph, MainWorktree, PlacementFacts, WorkTreeFacts};
pub use query::{Named, anchors_in, normalize_path};
pub use schema::{
    AcceptedText, AcceptedVia, Anchor, AnchorFault, COMPONENTS_DIR, Component, DECISIONS_DIR,
    Decision, FORMAT_VERSION, PROJECT_COMPONENT, PROJECT_FILE, Placement, Project, STATE_DIR,
    STORE_DIR, Status,
};
pub use state::{NodeFile, ProjectState, has_control_chars, is_valid_kebab_case};
pub use time::{Timestamp, TimestampError};
pub use validate::{Issue, IssueKind};
pub use version::FormatVersion;

/// Handle to a graph directory and the code root it is read for. It holds
/// no graph: a read loads one with [`Store::load_state`], and every graph
/// write goes through the [`Writer`] that [`Store::begin_write`] returns.
///
/// The two roots differ when a tree reaches a graph it does not hold: a
/// symlinked clone, a linked worktree of a shared graph. Anchors resolve
/// against the code root, and `.state/`, the lock and the journal, lives in
/// the graph directory, so every tree that reaches one graph shares one
/// lock.
#[derive(Debug)]
pub struct Store {
    pub(super) root: PathBuf,
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

    pub(crate) fn state_dir(&self) -> PathBuf {
        self.root.join(STATE_DIR)
    }

    pub(crate) fn tmp_dir(&self) -> PathBuf {
        self.state_dir().join("tmp")
    }

    pub(crate) fn lock_path(&self) -> PathBuf {
        self.state_dir().join("lock")
    }

    pub fn component_path(&self, name: &str) -> PathBuf {
        self.root.join(COMPONENTS_DIR).join(format!("{name}.toml"))
    }

    pub fn decision_path(&self, name: &str) -> PathBuf {
        self.root.join(DECISIONS_DIR).join(format!("{name}.toml"))
    }

    /// The path of `file` in this store.
    pub(crate) fn path_of(&self, file: &NodeFile) -> PathBuf {
        self.root.join(file.to_string())
    }

    /// Verify that `path` is inside the store root directory.
    /// Defense-in-depth: all store paths are derived from `self.root`, so
    /// this check should never fire in correct code. It guards against
    /// programming errors that would write or delete files outside `.trurlic/`.
    fn verify_path(&self, path: &Path) -> Result<()> {
        // Reject parent-directory components before the prefix check.
        // `starts_with` on non-canonicalized paths does not prevent
        // traversal via `..` segments (e.g. `/root/../etc/shadow`).
        for component in path.components() {
            if component == std::path::Component::ParentDir {
                return Err(Error::Validation(format!(
                    "path contains parent-directory traversal: {}",
                    path.display()
                )));
            }
        }
        if path.starts_with(&self.root) {
            Ok(())
        } else {
            Err(Error::Validation(format!(
                "path escapes store root: {}",
                path.display()
            )))
        }
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
            Err(Error::Validation(_))
        ));
        let traversal = store.root().join("..").join("escape.toml");
        let err = store.verify_path(&traversal).unwrap_err();
        assert!(err.to_string().contains("parent-directory"), "{err}");
    }
}
