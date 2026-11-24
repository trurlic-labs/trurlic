//! The commit journal, `.state/txn.toml`.
//!
//! A commit stages its files, writes the journal, then applies it: each
//! staged file renamed onto its target, the deleted files removed, the
//! directories flushed, the history commit made when the journal carries
//! its message (a `shared` graph's), the journal removed. The journal's
//! rename into place is the commit point. From then on the write lands: if
//! the writer stops or fails before the journal is gone, the next process
//! to take the exclusive lock finishes it before it loads, its history
//! commit included.
//!
//! Applying is idempotent. A write whose staged file is gone was renamed
//! already, removing a file that is gone is done, and a history commit
//! whose files the last commit holds as they are is made already. The
//! journal's deletion is flushed before the writer returns and releases
//! the lock, so a journal is on disk exactly while its write is not
//! finished: one that came back after a power loss would remove a file
//! re-created since and commit a later hand edit as the write's.

use std::collections::BTreeSet;
use std::fs;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::console::diag;
use crate::{Error, Result};

use super::commit::PendingWrite;
use super::durable::{place, sync_dir};
use super::failpoint::{self, Site};
use super::history::{CommitMessage, Head};
use super::{Store, StoreLock};

/// A write's file operations and its history commit. Paths are relative to
/// the store root.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Journal {
    /// Renamed in order.
    #[serde(default, rename = "write", skip_serializing_if = "Vec::is_empty")]
    writes: Vec<StagedWrite>,
    /// Removed after every write is in place.
    #[serde(default, rename = "remove", skip_serializing_if = "Vec::is_empty")]
    removes: Vec<PathBuf>,
    /// The message of the commit to the graph's own repository, made once
    /// the files are in place; `None` under `tracked`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    history: Option<CommitMessage>,
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

/// What a replay of a leftover journal did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Replayed {
    /// No journal was left.
    Nothing,
    /// The write is finished: its files in place, its history commit made
    /// when it has one, the journal gone.
    Finished,
    /// The write's files are in place, and its history commit could not
    /// be made: the journal stays for the next writer.
    HistoryWaits,
}

/// What a recovery does when the history commit of the write it finishes
/// fails, with the write's files in place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PendingHistory {
    /// Fail: a writer must not journal over a write not finished.
    Fail,
    /// Warn and keep the journal for the next writer: a read sees the
    /// files whole, and the commit waits.
    Keep,
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

    /// Every path the write changes: its targets, then its removals.
    fn paths(&self) -> Vec<PathBuf> {
        self.writes
            .iter()
            .map(|write| &write.target)
            .chain(&self.removes)
            // clone: the history step stages paths the journal keeps.
            .cloned()
            .collect()
    }
}

/// The bytes of the file at `path`, `None` when it is gone, refused
/// unopened when it is no regular file once its symlinks are followed:
/// a replay reads names a stopped writer left, which anyone could have
/// replaced since.
fn read_if_regular(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => {
            return Err(Error::NotAFile {
                path: path.to_path_buf(),
            });
        }
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(Error::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    }
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(source) => Err(Error::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// BLAKE3 of `bytes`, as lowercase hex.
fn hash_bytes(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

impl Store {
    pub(super) fn journal_path(&self) -> PathBuf {
        self.state_dir().join("txn.toml")
    }

    /// Whether an interrupted commit left a journal to apply.
    pub(super) fn has_journal(&self) -> Result<bool> {
        let path = self.journal_path();
        path.try_exists().map_err(Error::io(&path))
    }

    /// Stage `writes` and describe them, with `removes` and the message of
    /// the history commit, in a journal that is not written yet. On failure
    /// nothing stays staged.
    pub(super) fn stage_commit(
        &self,
        writes: &[PendingWrite],
        removes: &[PathBuf],
        history: Option<&CommitMessage>,
    ) -> Result<Journal> {
        let tmp_dir = self.tmp_dir();
        fs::create_dir_all(&tmp_dir).map_err(Error::io(&tmp_dir))?;
        let mut journal = Journal {
            writes: Vec::with_capacity(writes.len()),
            removes: removes
                .iter()
                .map(|path| self.relative(path))
                .collect::<Result<_>>()?,
            // clone: the journal is written from its own copy of the message.
            history: history.cloned(),
        };
        // In parallel: each staged file waits for its flush, and a commit
        // such as a migration's stages hundreds. The journal keeps the
        // order of `writes`.
        let staged: Vec<Result<StagedWrite>> = writes
            .par_iter()
            .map(|write| self.stage_write(write))
            .collect();
        let mut failure = None;
        for staged in staged {
            match staged {
                Ok(staged) => journal.writes.push(staged),
                Err(e) => {
                    failure.get_or_insert(e);
                }
            }
        }
        if let Some(e) = failure {
            self.discard(&journal);
            return Err(e);
        }
        // The journal must not name a staged file a power loss can take back.
        sync_dir(&tmp_dir)
            .map_err(Error::io(&tmp_dir))
            .inspect_err(|_| self.discard(&journal))?;
        Ok(journal)
    }

    fn stage_write(&self, write: &PendingWrite) -> Result<StagedWrite> {
        let target = self.relative(&write.target)?;
        if let Some(parent) = write.target.parent() {
            fs::create_dir_all(parent).map_err(Error::io(parent))?;
        }
        let temp = self.stage(write.content.as_bytes())?;
        Ok(StagedWrite {
            temp: self.relative(&temp)?,
            target,
            blake3: hash_bytes(write.content.as_bytes()),
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
    /// discards the staged files; after it the write is decided, and a
    /// failure is [`Error::WritePending`].
    pub(super) fn write_journal(&self, _lock: &StoreLock, journal: &Journal) -> Result<()> {
        let path = self.journal_path();
        let placed = toml::to_string(journal)
            .map_err(|source| Error::TomlSerialize {
                // clone: `path` is still needed for the rename below.
                path: path.clone(),
                source,
            })
            .and_then(|text| self.stage(text.as_bytes()))
            .and_then(|staged| place(&staged, &path));
        if let Err(e) = placed {
            self.discard(journal);
            return Err(e);
        }
        let state_dir = self.state_dir();
        sync_dir(&state_dir).map_err(|e| self.pending(Error::io(&state_dir)(e)))
    }

    /// Apply `journal`, make its history commit, then remove it. `head` is
    /// what the history step found before the commit point. A failure
    /// leaves the journal for the next read or write.
    pub(super) fn apply_journal(
        &self,
        lock: &StoreLock,
        journal: &Journal,
        head: Option<Head>,
    ) -> Result<()> {
        self.apply(journal).map_err(|e| self.pending(e))?;
        self.finish(lock, journal.history.as_ref(), &journal.paths(), head)
    }

    /// The history commit of the write whose files are in place, when it
    /// has one, then the journal removed.
    fn finish(
        &self,
        lock: &StoreLock,
        history: Option<&CommitMessage>,
        paths: &[PathBuf],
        head: Option<Head>,
    ) -> Result<()> {
        failpoint::hit(Site::Placed);
        if let Some(message) = history {
            self.commit_history(lock, message, paths, head)
                .map_err(|e| self.pending(e))?;
            failpoint::hit(Site::HistoryCommitted);
        }
        let journal = self.journal_path();
        if let Err(e) = fs::remove_file(&journal) {
            diag!(
                "warning: the write is applied, but {} remains: {e}",
                journal.display()
            );
            return Ok(());
        }
        // Flushed under the lock: no writer or reader after this one can
        // act before the deletion is on disk.
        let state_dir = self.state_dir();
        if let Err(e) = failpoint::fail(Site::Forgotten).and_then(|()| sync_dir(&state_dir)) {
            diag!(
                "warning: the write is applied, but the deletion of {} is not flushed: {e}; \
                 after a power loss it would be finished again",
                journal.display()
            );
        }
        failpoint::hit(Site::Finished);
        Ok(())
    }

    fn apply(&self, journal: &Journal) -> Result<()> {
        for write in &journal.writes {
            let target = self.root.join(&write.target);
            failpoint::fail(Site::Rename)
                .and_then(|()| fs::rename(self.root.join(&write.temp), &target))
                .map_err(Error::io(&target))?;
            failpoint::hit(Site::Applied);
        }
        for path in &journal.removes {
            let path = self.root.join(path);
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == ErrorKind::NotFound => {}
                Err(source) => return Err(Error::Io { path, source }),
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
            let dir = self.root.join(dir);
            sync_dir(&dir).map_err(Error::io(&dir))?;
        }
        Ok(())
    }

    /// Finish the write an interrupted commit left, if there is one, and
    /// report what was done. `pending` says what a history commit that
    /// fails then does.
    pub(super) fn replay_journal(
        &self,
        lock: &StoreLock,
        pending: PendingHistory,
    ) -> Result<Replayed> {
        let Some(journal) = self.read_journal()? else {
            return Ok(Replayed::Nothing);
        };
        // Every path of the write, the ones renamed before the stop too.
        let paths = journal.paths();
        let unapplied = self.unapplied(journal)?;
        self.apply(&unapplied).map_err(|e| self.pending(e))?;
        match self.finish(lock, unapplied.history.as_ref(), &paths, None) {
            Ok(()) => Ok(Replayed::Finished),
            Err(e) if pending == PendingHistory::Keep => {
                diag!("warning: {e}");
                Ok(Replayed::HistoryWaits)
            }
            Err(e) => Err(e),
        }
    }

    fn read_journal(&self) -> Result<Option<Journal>> {
        let Some(bytes) = read_if_regular(&self.journal_path())? else {
            return Ok(None);
        };
        let journal: Journal =
            toml::from_slice(&bytes).map_err(|e| self.bad_journal(e.to_string()))?;
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
            history: journal.history,
        })
    }

    /// The bytes of `path`, relative to the store root; `None` when it is gone.
    fn read_if_present(&self, path: &Path) -> Result<Option<Vec<u8>>> {
        read_if_regular(&self.root.join(path))
    }

    /// `path` relative to the store root, as the journal records it.
    fn relative(&self, path: &Path) -> Result<PathBuf> {
        path.strip_prefix(&self.root)
            .map(Path::to_path_buf)
            .map_err(|e| Error::Validation(format!("{}: {e}", path.display())))
    }

    fn pending(&self, source: Error) -> Error {
        Error::WritePending {
            journal: self.journal_path(),
            source: Box::new(source),
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
    use crate::store::history::Tool;
    use crate::store::testing::{
        graph_git, graph_log, sample_component, setup_store, shared_store,
    };
    use tempfile::TempDir;

    fn sample_journal() -> Journal {
        Journal {
            writes: vec![StagedWrite {
                temp: PathBuf::from(".state/tmp/4242-0"),
                target: PathBuf::from("components/auth.toml"),
                blake3: hash_bytes(b"description = \"\"\n"),
            }],
            removes: vec![PathBuf::from("decisions/use-jwt.toml")],
            history: None,
        }
    }

    /// Stage a component write, as a commit would, and return its journal.
    fn stage_component(store: &Store, name: &str) -> Journal {
        let write = store
            .prepare_write(&store.component_path(name), &sample_component())
            .unwrap();
        store.stage_commit(&[write], &[], None).unwrap()
    }

    #[test]
    fn journal_round_trips_with_and_without_removes_and_history() {
        let full = sample_journal();
        let text = toml::to_string(&full).unwrap();
        assert_eq!(toml::from_str::<Journal>(&text).unwrap(), full);

        let message = CommitMessage::new(Tool::Fmt, Some("row".into()), None).unwrap();
        let recorded = Journal {
            history: Some(message),
            ..sample_journal()
        };
        let text = toml::to_string(&recorded).unwrap();
        assert!(text.contains("[history]"), "{text}");
        assert_eq!(toml::from_str::<Journal>(&text).unwrap(), recorded);

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
                history: None,
            };
            fs::write(store.journal_path(), toml::to_string(&journal).unwrap()).unwrap();

            let err = store
                .replay_journal(&store.lock().unwrap(), PendingHistory::Fail)
                .unwrap_err();

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
        store.apply_journal(&lock, &journal, None).unwrap();

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

        assert_eq!(
            store.replay_journal(&lock, PendingHistory::Fail).unwrap(),
            Replayed::Finished
        );

        assert!(store.component_path("auth").exists());
        assert!(store.component_path("billing").exists());
        assert!(!store.has_journal().unwrap());
        assert_eq!(
            store.replay_journal(&lock, PendingHistory::Fail).unwrap(),
            Replayed::Nothing
        );
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

        assert_eq!(
            store.replay_journal(&lock, PendingHistory::Fail).unwrap(),
            Replayed::Finished
        );

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

        let err = store
            .replay_journal(&lock, PendingHistory::Fail)
            .unwrap_err();

        assert!(matches!(err, Error::BadJournal { .. }), "{err}");
        assert!(!store.component_path("auth").exists());
        assert!(store.has_journal().unwrap());
    }

    fn traced() -> CommitMessage {
        CommitMessage::new(Tool::Fmt, Some("plan row 7".into()), Some("s-1".into())).unwrap()
    }

    /// A write of the component `auth` to a shared store, stopped with its
    /// files in place and its history commit not made.
    fn stopped_after_its_renames(store: &Store) -> Journal {
        let lock = store.lock().unwrap();
        store.prepare_history(&lock).unwrap();
        let write = store
            .prepare_write(&store.component_path("auth"), &sample_component())
            .unwrap();
        let journal = store.stage_commit(&[write], &[], Some(&traced())).unwrap();
        store.write_journal(&lock, &journal).unwrap();
        store.apply(&journal).unwrap();
        journal
    }

    #[test]
    fn the_next_opener_makes_the_history_commit_with_the_original_trailers() {
        let tmp = TempDir::new().unwrap();
        let store = shared_store(tmp.path());
        stopped_after_its_renames(&store);

        store.load_state().unwrap();

        assert!(!store.has_journal().unwrap());
        assert_eq!(
            graph_log(&store),
            ["fmt\n\nOrigin: plan row 7\nSession: s-1", "init"]
        );
    }

    /// The commit landed before the stop, and was reworded and packed by
    /// hand since: the journal's files are in the last commit as they are,
    /// so the next opener commits nothing again.
    #[test]
    fn a_history_commit_made_before_the_stop_is_not_made_again() {
        let tmp = TempDir::new().unwrap();
        let store = shared_store(tmp.path());
        let journal = stopped_after_its_renames(&store);
        let lock = store.lock().unwrap();
        let paths = journal.paths();
        store
            .commit_history(&lock, &traced(), &paths, None)
            .unwrap();
        drop(lock);
        graph_git(&store, &["commit", "--quiet", "--amend", "-m", "reworded"]);
        graph_git(&store, &["gc", "--quiet"]);

        drop(store.begin_write().unwrap());

        assert!(!store.has_journal().unwrap());
        assert_eq!(graph_log(&store), ["reworded", "init"]);
    }

    /// A replay that puts the files in place and cannot make the history
    /// commit says so, and keeps the journal; once the commit can be made,
    /// the replay finishes the write.
    #[test]
    fn a_replay_tells_a_finished_write_from_one_whose_commit_waits() {
        let tmp = TempDir::new().unwrap();
        let store = shared_store(tmp.path());
        stopped_after_its_renames(&store);
        let index_lock = store.root().join(".git/index.lock");
        fs::write(&index_lock, "").unwrap();
        let lock = store.lock().unwrap();

        let replayed = store.replay_journal(&lock, PendingHistory::Keep).unwrap();

        assert_eq!(replayed, Replayed::HistoryWaits);
        assert!(store.has_journal().unwrap());
        fs::remove_file(&index_lock).unwrap();
        let replayed = store.replay_journal(&lock, PendingHistory::Keep).unwrap();
        assert_eq!(replayed, Replayed::Finished);
        assert!(!store.has_journal().unwrap());
    }

    /// A read finds the history commit cannot be made: it sees the write's
    /// files and leaves the journal, which refuses the next writer until
    /// the commit can be made.
    #[test]
    fn a_read_goes_on_past_a_history_commit_that_waits_and_a_writer_does_not() {
        let tmp = TempDir::new().unwrap();
        let store = shared_store(tmp.path());
        stopped_after_its_renames(&store);
        let index_lock = store.root().join(".git/index.lock");
        fs::write(&index_lock, "").unwrap();

        let state = store.load_state().unwrap();

        assert!(state.components.contains_key("auth"));
        assert!(store.has_journal().unwrap());
        let err = store.begin_write().map(drop).unwrap_err();
        assert!(matches!(err, Error::WritePending { .. }), "{err}");
        fs::remove_file(&index_lock).unwrap();
        drop(store.begin_write().unwrap());
        assert!(!store.has_journal().unwrap());
        assert_eq!(graph_log(&store).len(), 2);
    }
}
