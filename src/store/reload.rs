//! Loading the graph under the file lock.
//!
//! A write validates against the graph on disk, never against a state loaded
//! before its lock: another process may have committed in between, and a
//! commit built on the older state would erase that commit. A watcher loads
//! under the shared lock, so it never reads a commit halfway through its
//! renames. Either one first finishes a commit an interrupted writer left.

use crate::Result;

use super::failpoint::{self, Site};
use super::{ProjectState, Store, StoreLock};

impl Store {
    /// Take the guard `take_guard` returns, then the exclusive lock, then
    /// recover and load the graph under it. A server passes its state write
    /// lock as the guard and replaces the state it guards with the loaded
    /// one; the CLI passes `|| ()`.
    pub(crate) fn begin_write<G>(
        &self,
        take_guard: impl FnMut() -> G,
    ) -> Result<(G, StoreLock, ProjectState)> {
        let (guard, lock) = self.lock_after(take_guard)?;
        let state = self.load_recovered(&lock)?;
        Ok((guard, lock, state))
    }

    /// Load the graph under the shared lock, released before returning so
    /// the caller can take its state lock without holding the file lock.
    ///
    /// A journal seen under the shared lock belongs to a commit that stopped
    /// before it finished, since a live commit holds the exclusive lock. The
    /// load then takes the exclusive lock and finishes it: loading around it
    /// would serve a graph halfway between two commits.
    pub(super) fn load_shared(&self) -> Result<ProjectState> {
        {
            let _shared = self.lock_shared()?;
            failpoint::hit(Site::WatcherReload);
            self.check_version()?;
            if !self.has_journal()? {
                return self.load_state();
            }
        }
        self.load_recovered(&self.lock()?)
    }

    /// A store written in another format is refused before anything in it
    /// is touched.
    fn load_recovered(&self, lock: &StoreLock) -> Result<ProjectState> {
        self.check_version()?;
        self.recover(lock)?;
        self.load_state()
    }
}

#[cfg(test)]
mod tests {
    use crate::store::testing::{setup_store, setup_store_with_version};
    use tempfile::TempDir;

    #[test]
    fn begin_write_loads_what_another_writer_committed() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let mut other = store.load_state().unwrap();
        let lock = store.lock().unwrap();
        store.add_component(&lock, &mut other, "auth", "").unwrap();
        drop(lock);

        let ((), _lock, state) = store.begin_write(|| ()).unwrap();

        assert!(state.components.contains_key("auth"));
        assert_eq!(state.generation, other.generation);
    }

    #[test]
    fn a_store_in_another_format_is_refused() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store_with_version(tmp.path(), "0.1.0");

        let begun = store.begin_write(|| ());
        let err = begun.map(|_| ()).unwrap_err().to_string();
        assert!(err.contains("trurlic migrate"), "{err}");
        let err = store.load_shared().map(|_| ()).unwrap_err().to_string();
        assert!(err.contains("trurlic migrate"), "{err}");
    }
}
