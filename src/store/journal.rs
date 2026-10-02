//! The commit journal, `.state/txn.toml`.
//!
//! A commit stages its files, writes the journal, then applies it: each
//! staged file renamed onto its target (`graph.toml` last), the deleted
//! files removed, the directories flushed, the journal removed. The
//! journal's rename into place is the commit point. From then on the
//! commit lands: if the writer stops or fails before the journal is gone,
//! the next process to take the exclusive lock applies it before it loads.
//!
//! Applying is idempotent. A write whose staged file is gone was renamed
//! already, and removing a file that is gone is done.

use std::collections::BTreeSet;
use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

use super::commit::PendingWrite;
use super::durable::sync_dir;
use super::failpoint::{self, Site};
use super::{Store, StoreLock, hash_bytes};

/// A commit's file operations. Paths are relative to the store root.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Journal {
    /// Renamed in order, `graph.toml` last.
    #[serde(default, rename = "write", skip_serializing_if = "Vec::is_empty")]
    writes: Vec<StagedWrite>,
    /// Removed after every write is in place.
    #[serde(default, rename = "remove", skip_serializing_if = "Vec::is_empty")]
    removes: Vec<PathBuf>,
}

/// A staged file and where it goes.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct StagedWrite {
    temp: PathBuf,
    target: PathBuf,
    /// BLAKE3 of the staged bytes. A temp name recurs once process ids
    /// wrap, so a replay renames only the bytes this commit staged.
    blake3: String,
}

impl Journal {
    /// The first path that is not a plain path below the store root.
    fn escaping_path(&self) -> Option<&Path> {
        self.writes
            .iter()
            .flat_map(|write| [&write.temp, &write.target])
            .chain(&self.removes)
            .map(PathBuf::as_path)
            .find(|path| {
                let mut components = path.components().peekable();
                components.peek().is_none()
                    || !components.all(|component| matches!(component, Component::Normal(_)))
            })
    }
}

impl Store {
    pub(super) fn journal_path(&self) -> PathBuf {
        self.state_dir().join("txn.toml")
    }

    /// Whether an interrupted commit left a journal to apply.
    pub(super) fn has_journal(&self) -> Result<bool> {
        Ok(self.journal_path().try_exists()?)
    }

    /// Stage `writes` and describe them, with `removes`, in a journal that
    /// is not written yet. On failure nothing stays staged.
    pub(super) fn stage_commit(
        &self,
        writes: &[PendingWrite],
        removes: &[PathBuf],
    ) -> Result<Journal> {
        fs::create_dir_all(self.tmp_dir())?;
        let mut journal = Journal {
            writes: Vec::with_capacity(writes.len()),
            removes: removes
                .iter()
                .map(|path| self.relative(path))
                .collect::<Result<_>>()?,
        };
        for write in writes {
            let staged = self
                .stage_write(write)
                .inspect_err(|_| self.discard(&journal))?;
            journal.writes.push(staged);
        }
        // The journal must not name a staged file a power loss can take back.
        sync_dir(&self.tmp_dir()).inspect_err(|_| self.discard(&journal))?;
        Ok(journal)
    }

    fn stage_write(&self, write: &PendingWrite) -> Result<StagedWrite> {
        let target = self.relative(&write.target)?;
        if let Some(parent) = write.target.parent() {
            fs::create_dir_all(parent)?;
        }
        let temp = self.stage(write.content.as_bytes())?;
        Ok(StagedWrite {
            temp: self.relative(&temp)?,
            target,
            blake3: write.content_hash(),
        })
    }

    /// Remove what a commit staged, when it stops before its journal is in
    /// place. A file left behind goes at the next recovery.
    pub(super) fn discard(&self, journal: &Journal) {
        for write in &journal.writes {
            let _ = fs::remove_file(self.root.join(&write.temp));
        }
    }

    /// Put `journal` in place, which commits it. Before the rename a failure
    /// discards the staged files; after it the commit is decided, and a
    /// failure is [`Error::CommitPending`].
    pub(super) fn write_journal(&self, _lock: &StoreLock, journal: &Journal) -> Result<()> {
        let placed = toml::to_string(journal)
            .map_err(Error::from)
            .and_then(|text| self.stage(text.as_bytes()))
            .and_then(|staged| {
                fs::rename(&staged, self.journal_path()).map_err(|e| {
                    let _ = fs::remove_file(&staged);
                    Error::from(e)
                })
            });
        if let Err(e) = placed {
            self.discard(journal);
            return Err(e);
        }
        // Also makes the generation raised before the journal durable: both
        // live in `.state/`.
        sync_dir(&self.state_dir()).map_err(|e| self.pending(e))
    }

    /// Apply `journal`, then remove it. A failure leaves the journal for the
    /// next process that takes the lock.
    pub(super) fn apply_journal(&self, _lock: &StoreLock, journal: &Journal) -> Result<()> {
        self.apply(journal).map_err(|e| self.pending(e))?;
        // Not flushed: a journal that reappears after a power loss applies
        // again as a no-op.
        if let Err(e) = fs::remove_file(self.journal_path()) {
            eprintln!(
                "warning: the commit is applied, but {} remains: {e}",
                self.journal_path().display()
            );
        }
        Ok(())
    }

    fn apply(&self, journal: &Journal) -> io::Result<()> {
        for write in &journal.writes {
            failpoint::fail(Site::Rename)?;
            fs::rename(self.root.join(&write.temp), self.root.join(&write.target))?;
            failpoint::hit(Site::Applied);
        }
        for path in &journal.removes {
            match fs::remove_file(self.root.join(path)) {
                Ok(()) => {}
                Err(e) if e.kind() == ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
            failpoint::hit(Site::Applied);
        }
        let dirs: BTreeSet<&Path> = journal
            .writes
            .iter()
            .map(|write| &write.target)
            .chain(&journal.removes)
            .filter_map(|path| path.parent())
            .collect();
        for dir in dirs {
            sync_dir(&self.root.join(dir))?;
        }
        Ok(())
    }

    /// Apply the journal an interrupted commit left, if there is one, and
    /// report whether there was.
    pub(super) fn replay_journal(&self, lock: &StoreLock) -> Result<bool> {
        let Some(journal) = self.read_journal()? else {
            return Ok(false);
        };
        let unapplied = self.unapplied(journal)?;
        self.apply_journal(lock, &unapplied)?;
        Ok(true)
    }

    fn read_journal(&self) -> Result<Option<Journal>> {
        let text = match fs::read_to_string(self.journal_path()) {
            Ok(text) => text,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let journal: Journal =
            toml::from_str(&text).map_err(|e| self.bad_journal(e.to_string()))?;
        if let Some(path) = journal.escaping_path() {
            return Err(self.bad_journal(format!("`{}` leaves the store", path.display())));
        }
        Ok(Some(journal))
    }

    /// `journal` without the writes already renamed into place: those whose
    /// staged file is gone, and those whose target holds the staged bytes
    /// while the temp name belongs to another write's leftover.
    fn unapplied(&self, journal: Journal) -> Result<Journal> {
        let mut writes = Vec::with_capacity(journal.writes.len());
        for write in journal.writes {
            let Some(staged) = self.read_if_present(&write.temp)? else {
                continue;
            };
            if hash_bytes(&staged) == write.blake3 {
                writes.push(write);
            } else if self
                .read_if_present(&write.target)?
                .is_none_or(|bytes| hash_bytes(&bytes) != write.blake3)
            {
                return Err(self.bad_journal(format!(
                    "`{}` does not hold the bytes staged for `{}`",
                    write.temp.display(),
                    write.target.display()
                )));
            }
        }
        Ok(Journal {
            writes,
            removes: journal.removes,
        })
    }

    /// The bytes of `path`, relative to the store root; `None` when it is gone.
    fn read_if_present(&self, path: &Path) -> Result<Option<Vec<u8>>> {
        match fs::read(self.root.join(path)) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// `path` relative to the store root, as the journal records it.
    fn relative(&self, path: &Path) -> Result<PathBuf> {
        path.strip_prefix(&self.root)
            .map(Path::to_path_buf)
            .map_err(|e| Error::Validation(format!("{}: {e}", path.display())))
    }

    fn pending(&self, source: io::Error) -> Error {
        Error::CommitPending {
            journal: self.journal_path(),
            source,
        }
    }

    fn bad_journal(&self, detail: String) -> Error {
        Error::BadJournal {
            journal: self.journal_path(),
            detail,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testing::{sample_component, setup_store};
    use tempfile::TempDir;

    fn sample_journal() -> Journal {
        Journal {
            writes: vec![StagedWrite {
                temp: PathBuf::from(".state/tmp/4242-0"),
                target: PathBuf::from("graph.toml"),
                blake3: hash_bytes(b"graph"),
            }],
            removes: vec![PathBuf::from("decisions/use-jwt.toml")],
        }
    }

    /// Stage a component write, as a commit would, and return its journal.
    fn stage_component(store: &Store, name: &str) -> Journal {
        let write = store
            .prepare_write(&store.component_path(name), &sample_component(name))
            .unwrap();
        store.stage_commit(&[write], &[]).unwrap()
    }

    #[test]
    fn journal_round_trips_with_and_without_removes() {
        let full = sample_journal();
        let text = toml::to_string(&full).unwrap();
        assert_eq!(toml::from_str::<Journal>(&text).unwrap(), full);

        let writes_only = Journal {
            removes: vec![],
            ..sample_journal()
        };
        let text = toml::to_string(&writes_only).unwrap();
        assert!(!text.contains("remove"), "{text}");
        assert_eq!(toml::from_str::<Journal>(&text).unwrap(), writes_only);
    }

    #[test]
    fn a_journal_path_outside_the_store_is_refused() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        for removed in ["../outside.toml", "/etc/passwd", ""] {
            let journal = Journal {
                writes: vec![],
                removes: vec![PathBuf::from(removed)],
            };
            fs::write(store.journal_path(), toml::to_string(&journal).unwrap()).unwrap();

            let err = store.replay_journal(&store.lock().unwrap()).unwrap_err();

            assert!(matches!(err, Error::BadJournal { .. }), "{removed}: {err}");
            assert!(err.to_string().contains("txn.toml"), "{err}");
        }
    }

    #[test]
    fn a_staged_commit_applies_and_leaves_no_journal() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();
        let journal = stage_component(&store, "auth");

        store.write_journal(&lock, &journal).unwrap();
        store.apply_journal(&lock, &journal).unwrap();

        assert!(store.component_path("auth").exists());
        assert!(!store.has_journal().unwrap());
        assert_eq!(fs::read_dir(store.tmp_dir()).unwrap().count(), 0);
    }

    /// A replay renames what is still staged and skips what is in place.
    #[test]
    fn replay_finishes_a_partly_applied_commit() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();
        let mut journal = stage_component(&store, "auth");
        journal
            .writes
            .extend(stage_component(&store, "billing").writes);
        store.write_journal(&lock, &journal).unwrap();
        let first = &journal.writes[0];
        fs::rename(store.root.join(&first.temp), store.root.join(&first.target)).unwrap();

        assert!(store.replay_journal(&lock).unwrap());

        assert!(store.component_path("auth").exists());
        assert!(store.component_path("billing").exists());
        assert!(!store.has_journal().unwrap());
        assert!(!store.replay_journal(&lock).unwrap());
    }

    /// The journal of an applied commit reappears, and its temp name now
    /// holds another write's leftover: the commit is in place, so the replay
    /// leaves the target alone.
    #[test]
    fn replay_skips_a_write_in_place_whose_temp_name_was_reused() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();
        let journal = stage_component(&store, "auth");
        store.write_journal(&lock, &journal).unwrap();
        let write = &journal.writes[0];
        let target = store.root.join(&write.target);
        fs::rename(store.root.join(&write.temp), &target).unwrap();
        let committed = fs::read(&target).unwrap();
        fs::write(store.root.join(&write.temp), "another write").unwrap();

        assert!(store.replay_journal(&lock).unwrap());

        assert_eq!(fs::read(&target).unwrap(), committed);
        assert!(!store.has_journal().unwrap());
    }

    #[test]
    fn replay_refuses_a_staged_file_with_other_bytes() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();
        let journal = stage_component(&store, "auth");
        store.write_journal(&lock, &journal).unwrap();
        fs::write(store.root.join(&journal.writes[0].temp), "another write").unwrap();

        let err = store.replay_journal(&lock).unwrap_err();

        assert!(matches!(err, Error::BadJournal { .. }), "{err}");
        assert!(!store.component_path("auth").exists());
        assert!(store.has_journal().unwrap());
    }
}
