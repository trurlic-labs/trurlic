//! Finishing what an interrupted write left in `.state/`: the journal of a
//! write that did not complete, its history commit included, then temp
//! files.
//!
//! Only the holder of the exclusive lock touches either: a commit stages
//! its files in `.state/tmp/` and applies its journal under the lock, so a
//! cleanup without it could delete a staged file mid-commit. Under the lock
//! no commit is in flight, a journal is a commit to finish, and once it is
//! applied every temp file is a leftover. Every write recovers before it
//! loads, and so does a read that finds a leftover under the shared lock.

use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;

use crate::console::diag;
use crate::{Error, Result};

use super::journal::{PendingHistory, Replayed};
use super::lock::StoreLock;
use crate::store::format::Store;
use crate::store::format::read_dir_if_present;

impl Store {
    /// Finish the write an interrupted commit left, its history commit
    /// included, then remove the temp files left behind.
    pub(super) fn recover(&self, lock: &StoreLock) -> Result<()> {
        self.recover_with(lock, PendingHistory::Fail).map(drop)
    }

    /// [`recover`](Self::recover), with `pending` saying what a history
    /// commit that fails does; returns what the replay of the journal did.
    pub(super) fn recover_with(
        &self,
        lock: &StoreLock,
        pending: PendingHistory,
    ) -> Result<Replayed> {
        let replayed = self.replay_journal(lock, pending)?;
        match replayed {
            Replayed::Nothing => {}
            Replayed::Finished => diag!("warning: finished a write an interrupted commit left"),
            Replayed::HistoryWaits => diag!(
                "warning: the files of a write an interrupted commit left are in place; its \
                 history commit waits for the next write"
            ),
        }
        let removed = self.remove_temps(lock)?;
        if removed > 0 {
            diag!("warning: removed {removed} temp file(s) left by an interrupted write");
        }
        Ok(replayed)
    }

    /// Whether an interrupted write left a journal or a temp file. Asked
    /// under the shared lock, when no commit is in flight, so either one is
    /// a leftover.
    pub(super) fn has_leftovers(&self) -> Result<bool> {
        Ok(self.has_journal()? || self.leftover_temps()?.next().transpose()?.is_some())
    }

    fn remove_temps(&self, _lock: &StoreLock) -> Result<usize> {
        let mut removed = 0;
        for path in self.leftover_temps()? {
            let path = path?;
            match fs::remove_file(&path) {
                Ok(()) => removed += 1,
                Err(e) if e.kind() == ErrorKind::NotFound => {}
                Err(source) => return Err(Error::Io { path, source }),
            }
        }
        Ok(removed)
    }

    /// The files in `.state/tmp/`, which only a commit in flight writes:
    /// what recovery removes, and what makes a read recover first.
    fn leftover_temps(&self) -> Result<impl Iterator<Item = Result<PathBuf>>> {
        let tmp_dir = self.tmp_dir();
        let entries = read_dir_if_present(&tmp_dir)?;
        Ok(entries.into_iter().flatten().filter_map(move |entry| {
            let file = entry.and_then(|entry| {
                let is_file = entry.file_type()?.is_file();
                Ok(is_file.then(|| entry.path()))
            });
            file.map_err(Error::io(&tmp_dir)).transpose()
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testing::setup_store;
    use tempfile::TempDir;

    fn leave_temps(store: &Store) {
        fs::create_dir_all(store.tmp_dir()).unwrap();
        fs::write(store.tmp_dir().join("4242-0"), "staged").unwrap();
        fs::write(store.tmp_dir().join("4242-1"), "staged").unwrap();
    }

    fn temp_count(store: &Store) -> usize {
        fs::read_dir(store.tmp_dir()).unwrap().count()
    }

    #[test]
    fn recover_removes_every_temp_file() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        leave_temps(&store);

        store.recover(&store.lock().unwrap()).unwrap();

        assert_eq!(temp_count(&store), 0);
    }

    #[test]
    fn recover_without_a_temp_dir_succeeds() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());

        store.recover(&store.lock().unwrap()).unwrap();
    }

    #[test]
    fn leftovers_are_a_journal_or_a_temp_file() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        assert!(!store.has_leftovers().unwrap());
        // A directory is not a file recovery removes.
        fs::create_dir_all(store.tmp_dir().join("subdir")).unwrap();
        assert!(!store.has_leftovers().unwrap());

        leave_temps(&store);
        assert!(store.has_leftovers().unwrap());

        store.recover(&store.lock().unwrap()).unwrap();
        assert!(!store.has_leftovers().unwrap());

        fs::write(store.journal_path(), "").unwrap();
        assert!(store.has_leftovers().unwrap());
    }
}
