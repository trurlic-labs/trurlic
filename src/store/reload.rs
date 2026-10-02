//! Loading the graph under the file lock.
//!
//! A write validates against the graph on disk, never against a state loaded
//! before its lock: another process may have committed in between, and a
//! commit built on the older state would erase that commit.

use crate::Result;

use super::{ProjectState, Store, StoreLock};

impl Store {
    /// Take the guard `take_guard` returns, then the exclusive lock, then
    /// load the graph under it. A server passes its state write lock as the
    /// guard and replaces the state it guards with the loaded one; the CLI
    /// passes `|| ()`.
    pub(crate) fn begin_write<G>(
        &self,
        take_guard: impl FnMut() -> G,
    ) -> Result<(G, StoreLock, ProjectState)> {
        let (guard, lock) = self.lock_after(take_guard)?;
        let state = self.load_checked()?;
        Ok((guard, lock, state))
    }

    /// A store written in another format is refused before its node files
    /// are parsed.
    fn load_checked(&self) -> Result<ProjectState> {
        self.check_version()?;
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
    }
}
