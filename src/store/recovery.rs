//! Finishing what an interrupted write left in `.state/`: the journal of a
//! commit that did not complete, then temp files.
//!
//! Only the holder of the exclusive lock touches either: a commit stages
//! its files in `.state/tmp/` and applies its journal under the lock, so a
//! cleanup without it could delete a staged file mid-commit. Under the lock
//! no commit is in flight, a journal is a commit to finish, and once it is
//! applied every temp file is a leftover.

use std::fs;
use std::io::ErrorKind;

use crate::Result;
use crate::console::diag;

use super::{Store, StoreLock};

impl Store {
    /// Apply the journal an interrupted commit left, then remove the temp
    /// files left behind.
    pub(crate) fn recover(&self, lock: &StoreLock) -> Result<()> {
        if self.replay_journal(lock)? {
            diag!("warning: applied a commit an interrupted write left unfinished");
        }
        let removed = self.remove_temps()?;
        if removed > 0 {
            diag!("warning: removed {removed} temp file(s) left by an interrupted write");
        }
        Ok(())
    }

    /// [`recover`](Self::recover) if the exclusive lock is free now. While
    /// another process holds it, its commit may be using `.state/`; the next
    /// writer recovers under its own lock.
    pub(crate) fn try_recover(&self) -> Result<()> {
        match self.try_lock()? {
            Some(lock) => self.recover(&lock),
            None => Ok(()),
        }
    }

    fn remove_temps(&self) -> Result<usize> {
        let entries = match fs::read_dir(self.tmp_dir()) {
            Ok(entries) => entries,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e.into()),
        };
        let mut removed = 0;
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            match fs::remove_file(entry.path()) {
                Ok(()) => removed += 1,
                Err(e) if e.kind() == ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(removed)
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
    fn try_recover_leaves_the_temps_of_a_lock_holder() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        leave_temps(&store);
        let held = store.lock().unwrap();

        store.try_recover().unwrap();
        assert_eq!(temp_count(&store), 2);

        drop(held);
        store.try_recover().unwrap();
        assert_eq!(temp_count(&store), 0);
    }
}
