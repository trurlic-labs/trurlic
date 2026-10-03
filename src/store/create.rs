//! Creating an empty store: the node directories, `project.toml` and an
//! index that holds only the `project` node.

use std::fs;
use std::path::Path;

use chrono::{DateTime, Utc};

use crate::{Error, Result};

use super::schema::{GraphIndex, NodeEntry, NodeKind, Project, ProjectFile};
use super::{
    COMPONENTS_DIR, DECISIONS_DIR, FORMAT_VERSION, PATTERNS_DIR, STORE_DIR, Store, hash_file,
};

impl Store {
    /// Create `.trurlic/` in `project_dir` for a project named `name`.
    /// `rebuilt` stamps the first index, and every later commit carries it
    /// over. Fails with [`Error::StoreExists`] when `.trurlic/` is there.
    pub fn create(project_dir: &Path, name: &str, rebuilt: DateTime<Utc>) -> Result<Self> {
        let root = project_dir.join(STORE_DIR);
        if root.exists() {
            return Err(Error::StoreExists(root));
        }
        for dir in [COMPONENTS_DIR, DECISIONS_DIR, PATTERNS_DIR] {
            let dir = root.join(dir);
            fs::create_dir_all(&dir).map_err(Error::io(&dir))?;
        }
        let store = Self::at(root);
        let tmp_dir = store.tmp_dir();
        fs::create_dir_all(&tmp_dir).map_err(Error::io(&tmp_dir))?;

        let lock = store.lock()?;
        let project_path = store.root().join("project.toml");
        let project = ProjectFile {
            trurlic_version: FORMAT_VERSION.into(),
            project: Project {
                name: name.into(),
                description: String::new(),
            },
        };
        store.write_atomic(&lock, &project_path, &project)?;
        let index = GraphIndex {
            version: 1,
            rebuilt: Some(rebuilt),
            nodes: vec![NodeEntry {
                name: "project".into(),
                kind: NodeKind::Component,
                tags: vec![],
                hash: hash_file(&project_path)?,
            }],
            edges: vec![],
        };
        store.write_atomic(&lock, &store.graph_path(), &index)?;
        Ok(store)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::STATE_DIR;
    use crate::store::testing::ts;
    use tempfile::TempDir;

    #[test]
    fn create_lays_out_an_empty_store_that_loads_without_issues() {
        let tmp = TempDir::new().unwrap();
        let store = Store::create(tmp.path(), "demo", ts()).unwrap();

        for dir in [COMPONENTS_DIR, DECISIONS_DIR, PATTERNS_DIR] {
            assert!(store.root().join(dir).is_dir(), "{dir}");
        }
        assert!(store.root().join(STATE_DIR).join("tmp").is_dir());
        let state = store.load_state().unwrap();
        assert_eq!(state.project.project.name, "demo");
        assert_eq!(state.project.trurlic_version, FORMAT_VERSION);
        assert_eq!(state.graph_index.rebuilt, Some(ts()));
        let names: Vec<&str> = state
            .graph_index
            .nodes
            .iter()
            .map(|n| n.name.as_str())
            .collect();
        assert_eq!(names, ["project"]);
        assert!(state.graph_index.edges.is_empty());
        assert!(state.validate().is_empty());
        assert!(store.verify_hashes().unwrap().is_empty());
    }

    #[test]
    fn create_refuses_an_existing_store() {
        let tmp = TempDir::new().unwrap();
        Store::create(tmp.path(), "demo", ts()).unwrap();

        let err = Store::create(tmp.path(), "demo", ts()).unwrap_err();

        assert!(matches!(err, Error::StoreExists(_)));
    }
}
