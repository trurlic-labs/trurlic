//! The two ways into the graph: a read of the files on disk, and a
//! [`Writer`] that commits to them.
//!
//! No process keeps the graph between calls. A read loads it under the
//! shared lock; a write loads it under the exclusive lock and validates
//! against that load, which the writer owns with the lock, so no write can
//! be built on a graph loaded before another commit. Either one first
//! finishes what an interrupted commit left, so neither sees half of one.

use crate::Result;

use super::{ProjectFile, ProjectState, Store, StoreLock};

impl Store {
    /// The graph as the files on disk hold it, loaded under the shared lock.
    pub fn load_state(&self) -> Result<ProjectState> {
        self.read(|project, project_hash| self.read_graph(project, project_hash))
    }

    /// Take the exclusive lock, finish what an interrupted write left, and
    /// load the graph under the lock. Every graph write on every surface
    /// starts here and commits through the returned [`Writer`].
    pub fn begin_write(&self) -> Result<Writer<'_>> {
        let lock = self.lock()?;
        let (project, project_hash) = self.recover_current(&lock)?;
        let state = self.read_graph(project, project_hash)?;
        Ok(Writer {
            store: self,
            lock,
            state,
        })
    }

    /// Run `load` on `project.toml` and its hash while no commit can land,
    /// once the format is checked: a store in another format is refused
    /// before anything else in it is read.
    ///
    /// `load` runs under the shared lock. Under it no commit is in flight,
    /// so a journal or a temp file found there was left by a writer that
    /// stopped. `load` then runs under the exclusive lock, after that commit
    /// is finished: reading around it would see a graph halfway between two
    /// commits.
    pub(super) fn read<T>(&self, load: impl FnOnce(ProjectFile, String) -> Result<T>) -> Result<T> {
        {
            let _shared = self.lock_shared()?;
            let (project, project_hash) = self.read_current_project()?;
            if !self.has_leftovers()? {
                return load(project, project_hash);
            }
        }
        let lock = self.lock()?;
        let (project, project_hash) = self.recover_current(&lock)?;
        load(project, project_hash)
    }

    /// Check the format, then finish what an interrupted write left, so
    /// nothing in a store of another format is touched.
    fn recover_current(&self, lock: &StoreLock) -> Result<(ProjectFile, String)> {
        self.check_version()?;
        self.recover(lock)?;
        self.read_current_project()
    }
}

/// The exclusive lock and the graph loaded under it. Every graph write is a
/// method of a writer, so a write validates against the store as it is
/// while the lock is held, and each commit leaves the writer's graph equal
/// to the files on disk. Dropping the writer releases the lock.
#[must_use = "dropping the writer releases the lock"]
pub struct Writer<'s> {
    pub(super) store: &'s Store,
    pub(super) lock: StoreLock,
    pub(super) state: ProjectState,
}

impl<'s> Writer<'s> {
    /// The graph as the last commit, or the load, left it on disk.
    #[must_use]
    pub fn state(&self) -> &ProjectState {
        &self.state
    }

    #[must_use]
    pub fn store(&self) -> &'s Store {
        self.store
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::Error;
    use crate::store::testing::{sample_component, setup_store, setup_store_with_version};
    use tempfile::TempDir;

    #[test]
    fn a_writer_loads_what_an_earlier_writer_committed() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        store
            .begin_write()
            .unwrap()
            .add_component("auth", "")
            .unwrap();

        let writer = store.begin_write().unwrap();

        assert!(writer.state().components.contains_key("auth"));
    }

    #[test]
    fn a_load_finishes_a_commit_a_stopped_writer_journaled() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();
        let write = store
            .prepare_write(&store.component_path("auth"), &sample_component("auth"))
            .unwrap();
        let journal = store.stage_commit(&[write], &[]).unwrap();
        store.write_journal(&lock, &journal).unwrap();
        drop(lock);

        let state = store.load_state().unwrap();

        assert!(state.components.contains_key("auth"));
        assert!(!store.has_journal().unwrap());
    }

    #[test]
    fn a_load_waits_for_the_writer_that_holds_the_lock() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let mut writer = store.begin_write().unwrap();

        std::thread::scope(|scope| {
            let reader = scope.spawn(|| store.load_state());
            writer.add_component("auth", "").unwrap();
            drop(writer);
            let state = reader.join().unwrap().unwrap();
            assert!(state.components.contains_key("auth"));
        });
    }

    #[test]
    fn a_load_removes_the_temp_files_a_stopped_writer_left() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        fs::create_dir_all(store.tmp_dir()).unwrap();
        fs::write(store.tmp_dir().join("4242-0"), "staged").unwrap();

        store.load_state().unwrap();

        assert_eq!(fs::read_dir(store.tmp_dir()).unwrap().count(), 0);
    }

    #[test]
    fn a_store_in_another_format_is_refused() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store_with_version(tmp.path(), "0.1.0");

        let err = store.begin_write().map(|_| ()).unwrap_err();
        assert!(matches!(err, Error::VersionMismatch { .. }), "{err}");
        let err = store.load_state().map(|_| ()).unwrap_err();
        assert!(matches!(err, Error::VersionMismatch { .. }), "{err}");
    }
}
