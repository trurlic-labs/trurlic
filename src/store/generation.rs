//! The commit counter in `.state/generation`.
//!
//! Every commit raises it under the exclusive lock, and every locked load
//! reads it with the node files, so the generations of two loads order
//! them. A commit refuses a state whose generation is behind the store's:
//! that state was loaded before another commit, and committing it would
//! erase that commit. A watcher drops a load that a write of its own server
//! overtook (see [`ProjectState::is_overtaken`]).
//!
//! `.state/` is never authoritative, so a missing file reads as 0.

use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;

use crate::{Error, Result};

use super::{ProjectState, Store, StoreLock};

impl Store {
    fn generation_path(&self) -> PathBuf {
        self.state_dir().join("generation")
    }

    /// The generation on disk; 0 before the first commit records one.
    pub(super) fn read_generation(&self) -> Result<u64> {
        let path = self.generation_path();
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e.into()),
        };
        text.trim().parse().map_err(|e| {
            Error::Validation(format!(
                "{} does not hold a commit counter ({e}); delete it to reset the counter",
                path.display()
            ))
        })
    }

    /// Refuse `state` when a commit landed after it was loaded.
    pub(super) fn ensure_current(&self, _lock: &StoreLock, state: &ProjectState) -> Result<()> {
        let on_disk = self.read_generation()?;
        if state.generation == on_disk {
            Ok(())
        } else {
            Err(Error::StaleState {
                loaded: state.generation,
                on_disk,
            })
        }
    }

    /// Record the next generation and return it. Written to a temp file and
    /// renamed, so a reader sees the old counter or the new one.
    pub(super) fn raise_generation(&self, _lock: &StoreLock) -> Result<u64> {
        let next = self.read_generation()?.saturating_add(1);
        let staged = self.temp_path();
        fs::write(&staged, format!("{next}\n"))?;
        fs::rename(&staged, self.generation_path())?;
        Ok(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testing::setup_store;
    use tempfile::TempDir;

    #[test]
    fn each_commit_raises_the_generation_by_one() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();
        let mut state = store.load_state().unwrap();
        assert_eq!(state.generation, 0);

        store.add_component(&lock, &mut state, "auth", "").unwrap();
        store
            .add_component(&lock, &mut state, "billing", "")
            .unwrap();

        assert_eq!(state.generation, 2);
        assert_eq!(store.read_generation().unwrap(), 2);
        assert_eq!(store.load_state().unwrap().generation, 2);
    }

    #[test]
    fn a_state_loaded_before_another_commit_is_refused() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();
        let mut stale = store.load_state().unwrap();
        let mut current = store.load_state().unwrap();
        store
            .add_component(&lock, &mut current, "auth", "")
            .unwrap();

        let err = store
            .add_component(&lock, &mut stale, "billing", "")
            .unwrap_err();

        assert!(
            matches!(
                err,
                Error::StaleState {
                    loaded: 0,
                    on_disk: 1
                }
            ),
            "{err}"
        );
        assert!(!stale.components.contains_key("billing"));
        let on_disk = store.load_state().unwrap();
        assert!(on_disk.components.contains_key("auth"));
        assert!(!on_disk.components.contains_key("billing"));
    }

    #[test]
    fn a_corrupt_counter_names_its_file() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        fs::write(store.generation_path(), "seven").unwrap();

        let err = store.read_generation().unwrap_err().to_string();

        assert!(err.contains("generation"), "{err}");
        assert!(err.contains("delete it"), "{err}");
    }
}
