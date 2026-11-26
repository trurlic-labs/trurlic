//! Where each file of a graph lives: [`Store`], the graph directory and
//! the tree it is read for, and the paths of the files under it. Every
//! path a write touches is derived here, and one that would leave the
//! graph directory is refused before anything is written.

use std::fmt;
use std::path::{self, Path, PathBuf};

use crate::store::model::{
    COMPONENTS_DIR, DECISIONS_DIR, NodeFile, PROJECT_FILE, STATE_DIR, STORE_DIR,
};
use crate::{Error, Result};

/// Handle to a graph directory and the code root it is read for. It holds
/// no graph: a read loads one with [`Store::load_state`], and every write
/// to a current-format graph goes through the
/// [`Writer`](crate::store::Writer) that [`Store::begin_write`] returns.
///
/// The two roots differ when a tree reaches a graph it does not hold: a
/// symlinked clone, a linked worktree of a shared graph. Anchors resolve
/// against the code root, and `.state/`, the lock and the journal, lives in
/// the graph directory, so every tree that reaches one graph shares one
/// lock.
#[derive(Debug)]
pub struct Store {
    pub(in crate::store) root: PathBuf,
    pub(in crate::store) code_root: PathBuf,
}

impl Store {
    /// The graph at `root`, read for the directory that holds it.
    pub(crate) fn at(root: PathBuf) -> Self {
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
    pub(crate) fn code_root(&self) -> &Path {
        &self.code_root
    }

    /// Whether the graph directory is the code root's own `.trurlic/`, as
    /// opposed to one reached through a symlink or a linked worktree.
    pub(crate) fn is_local(&self) -> bool {
        self.root == self.code_root.join(STORE_DIR)
    }

    pub(in crate::store) fn state_dir(&self) -> PathBuf {
        self.root.join(STATE_DIR)
    }

    pub(in crate::store) fn tmp_dir(&self) -> PathBuf {
        self.state_dir().join("tmp")
    }

    pub(in crate::store) fn lock_path(&self) -> PathBuf {
        self.state_dir().join("lock")
    }

    pub(in crate::store) fn project_path(&self) -> PathBuf {
        self.root.join(PROJECT_FILE)
    }

    pub fn component_path(&self, name: &str) -> PathBuf {
        self.root.join(COMPONENTS_DIR).join(format!("{name}.toml"))
    }

    pub(crate) fn decision_path(&self, name: &str) -> PathBuf {
        self.root.join(DECISIONS_DIR).join(format!("{name}.toml"))
    }

    pub(in crate::store) fn path_of(&self, file: &NodeFile) -> PathBuf {
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
    pub(in crate::store) fn verify_path(&self, path: &Path) -> Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testing::*;
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
