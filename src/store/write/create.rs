//! Creating an empty store: the node directories and `project.toml`, and
//! under `shared` the graph's own repository with its first commit.

use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use crate::{Error, Result};

use crate::store::format::Store;
use crate::store::model::Quality;
use crate::store::model::{COMPONENTS_DIR, DECISIONS_DIR, FORMAT_VERSION, STORE_DIR};
use crate::store::model::{Placement, Project};

impl Store {
    /// Create `.trurlic/` in `project_dir` for a project named `name`. Fails
    /// with [`Error::StoreExists`] when `.trurlic/` is there, and leaves no
    /// `.trurlic/` behind when it fails otherwise, as it does when a
    /// `shared` graph finds no `git` to make its repository with.
    pub(crate) fn create(project_dir: &Path, name: &str, placement: Placement) -> Result<Self> {
        let root = project_dir.join(STORE_DIR);
        fs::create_dir_all(project_dir).map_err(Error::io(project_dir))?;
        match fs::create_dir(&root) {
            Err(e) if e.kind() == ErrorKind::AlreadyExists => return Err(Error::StoreExists(root)),
            created => created.map_err(Error::io(&root))?,
        }
        let store = Self::at(root);
        if let Err(e) = store.lay_out(name, placement) {
            // The directory is this call's own: it did not exist above.
            let _ = fs::remove_dir_all(store.root());
            return Err(e);
        }
        Ok(store)
    }

    /// Fill the new, empty graph directory. The repository of a shared
    /// graph comes first, so a missing `git` fails before any graph file is
    /// written.
    fn lay_out(&self, name: &str, placement: Placement) -> Result<()> {
        for dir in [COMPONENTS_DIR, DECISIONS_DIR] {
            let dir = self.root.join(dir);
            fs::create_dir_all(&dir).map_err(Error::io(&dir))?;
        }
        let tmp_dir = self.tmp_dir();
        fs::create_dir_all(&tmp_dir).map_err(Error::io(&tmp_dir))?;

        let lock = self.lock()?;
        if placement == Placement::Shared {
            self.create_repository(&lock)?;
        }
        let project = Project {
            format_version: FORMAT_VERSION,
            name: name.into(),
            description: String::new(),
            placement,
            quality: Quality::default(),
        };
        self.write_atomic(&lock, &self.project_path(), &project)?;
        if placement == Placement::Shared {
            self.commit_init(&lock)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::STATE_DIR;
    use tempfile::TempDir;

    #[test]
    fn create_lays_out_an_empty_store_that_loads_without_issues() {
        let tmp = TempDir::new().unwrap();
        let store = Store::create(tmp.path(), "demo", Placement::Shared).unwrap();

        for dir in [COMPONENTS_DIR, DECISIONS_DIR] {
            assert!(store.root().join(dir).is_dir(), "{dir}");
        }
        assert!(store.root().join(STATE_DIR).join("tmp").is_dir());
        assert!(!store.root().join("graph.toml").exists());
        let state = store.load_state().unwrap();
        assert_eq!(state.project.name, "demo");
        assert_eq!(state.project.placement, Placement::Shared);
        assert_eq!(state.project.format_version, FORMAT_VERSION);
        assert_eq!(state.validate(), []);
        assert_eq!(state.non_canonical, []);
    }

    #[test]
    fn create_refuses_an_existing_store() {
        let tmp = TempDir::new().unwrap();
        Store::create(tmp.path(), "demo", Placement::Tracked).unwrap();

        let err = Store::create(tmp.path(), "demo", Placement::Tracked).unwrap_err();

        assert!(matches!(err, Error::StoreExists(_)));
    }
}
