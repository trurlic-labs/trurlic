//! The commit journal, `.state/txn.toml`.
//!
//! A commit stages its files, writes the journal, then applies it: each
//! staged file renamed onto its target, the deleted files removed, the
//! directories flushed, the history commit made when the journal carries
//! its step (a `shared` graph's), the journal removed. The journal's
//! rename into place is the commit point. From then on the write lands: if
//! the writer stops or fails before the journal is gone, the next process
//! to take the exclusive lock finishes it before it loads, its history
//! commit included.
//!
//! Applying is idempotent. A write whose staged file is gone was renamed
//! already, removing a file that is gone is done, and a history commit the
//! branch holds is made already. Once the files are in place, a history
//! commit that fails narrows the journal to the history step alone, so no
//! later read or write removes a file again, which would take one
//! re-created since; a narrowing that fails is a warning, and the file step
//! runs again. The journal's deletion is flushed before the writer returns
//! and releases the lock; a deletion or a flush that fails is a warning,
//! and the finished write is replayed again.

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
use super::history::{Head, HistoryStep};
use super::lock::StoreLock;
use super::message::CommitMessage;
use crate::store::format::ensure_regular;
use crate::store::format::{Store, StorePathFault};

/// A write's file operations and its history commit. Paths are relative to
/// the store root.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Journal {
    /// Renamed in order.
    #[serde(default, rename = "write", skip_serializing_if = "Vec::is_empty")]
    writes: Vec<StagedWrite>,
    /// Removed after every write is in place.
    #[serde(default, rename = "remove", skip_serializing_if = "Vec::is_empty")]
    removes: Vec<PathBuf>,
    /// The commit to the graph's own repository, made once the files are
    /// in place; `None` under `tracked`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    history: Option<HistoryStep>,
}

/// A staged file and where it goes.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
    /// be made: a journal of that commit alone, or the whole journal when
    /// narrowing it failed, stays for the next writer.
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
        let history = self.history.iter().flat_map(HistoryStep::paths);
        self.writes
            .iter()
            .flat_map(|write| [&write.temp, &write.target])
            .chain(&self.removes)
            .map(PathBuf::as_path)
            .chain(history)
            .find(|path| {
                let mut components = path.components().peekable();
                components.peek().is_none()
                    || !components.all(|component| matches!(component, Component::Normal(_)))
            })
    }
}

/// The bytes of the file at `path`, `None` when it is gone, refused
/// unopened when it is no regular file once its symlinks are followed:
/// a replay reads names a stopped writer left, which anyone could have
/// replaced since.
fn read_if_regular(path: &Path) -> Result<Option<Vec<u8>>> {
    let gone = |e: &std::io::Error| e.kind() == ErrorKind::NotFound;
    match ensure_regular(path) {
        Ok(_) => {}
        Err(Error::Io { source, .. }) if gone(&source) => return Ok(None),
        Err(e) => return Err(e),
    }
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if gone(&e) => Ok(None),
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

    /// Stage `writes` and describe them, with `removes`, in a journal that
    /// is not written yet. On failure nothing stays staged.
    pub(super) fn stage_commit(
        &self,
        lock: &StoreLock,
        writes: &[PendingWrite],
        removes: &[PathBuf],
    ) -> Result<Journal> {
        let tmp_dir = self.tmp_dir();
        fs::create_dir_all(&tmp_dir).map_err(Error::io(&tmp_dir))?;
        let mut journal = Journal {
            writes: Vec::with_capacity(writes.len()),
            removes: removes
                .iter()
                .map(|path| self.relative(path))
                .collect::<Result<_>>()?,
            history: None,
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
            self.discard(lock, &journal);
            return Err(e);
        }
        // The journal must not name a staged file a power loss can take back.
        sync_dir(&tmp_dir)
            .map_err(Error::io(&tmp_dir))
            .inspect_err(|_| self.discard(lock, &journal))?;
        Ok(journal)
    }

    /// Give `journal` the history step of a write to a `shared` graph whose
    /// branch is at `head`: its staged files written to the graph's
    /// repository as blobs. On failure nothing stays staged.
    pub(super) fn attach_history(
        &self,
        lock: &StoreLock,
        journal: &mut Journal,
        message: &CommitMessage,
        head: &Head,
    ) -> Result<()> {
        let staged: Vec<&Path> = journal.writes.iter().map(|w| w.temp.as_path()).collect();
        let ids = match self.write_blobs(lock, &staged) {
            Ok(ids) => ids,
            Err(e) => {
                self.discard(lock, journal);
                return Err(e);
            }
        };
        // clone: the history step names each target the file step renames.
        let targets = journal.writes.iter().map(|write| write.target.clone());
        let step = HistoryStep::new(message, head, targets.zip(ids), &journal.removes);
        journal.history = Some(step);
        Ok(())
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
    fn discard(&self, _lock: &StoreLock, journal: &Journal) {
        for write in &journal.writes {
            let _ = fs::remove_file(self.root.join(&write.temp));
        }
    }

    /// Put `journal` in place, which commits it. Before the rename a failure
    /// discards the staged files; after it the write is decided, and a
    /// failure is [`Error::WritePending`].
    pub(super) fn write_journal(&self, lock: &StoreLock, journal: &Journal) -> Result<()> {
        if let Err(e) = self.place_journal(journal) {
            self.discard(lock, journal);
            return Err(e);
        }
        let state_dir = self.state_dir();
        sync_dir(&state_dir).map_err(|e| self.pending(Error::io(&state_dir)(e)))
    }

    /// `journal` staged and renamed onto `.state/txn.toml`, not flushed.
    fn place_journal(&self, journal: &Journal) -> Result<()> {
        let path = self.journal_path();
        let text = toml::to_string(journal).map_err(|source| Error::TomlSerialize {
            // clone: `path` is still needed for the rename below.
            path: path.clone(),
            source,
        })?;
        place(&self.stage(text.as_bytes())?, &path)
    }

    /// Apply `journal`, make its history commit, then remove it. `head` is
    /// what the history step found before the commit point. A failure
    /// leaves a journal of the steps still to do for the next read or
    /// write.
    pub(super) fn apply_journal(
        &self,
        lock: &StoreLock,
        journal: &Journal,
        head: Option<Head>,
    ) -> Result<()> {
        self.apply(lock, journal).map_err(|e| self.pending(e))?;
        self.finish(lock, journal, head)
    }

    /// The history commit of the write whose files are in place, when it
    /// has one, then the journal removed. When the commit fails, the
    /// journal is narrowed to the history step.
    fn finish(&self, lock: &StoreLock, journal: &Journal, head: Option<Head>) -> Result<()> {
        failpoint::hit(Site::Placed);
        if let Some(step) = &journal.history {
            if let Err(e) = self.commit_history(lock, step, head) {
                self.narrow(step);
                return Err(self.pending(e));
            }
            failpoint::hit(Site::HistoryCommitted);
        }
        let journal = self.journal_path();
        if let Err(e) = failpoint::fail(Site::Forget).and_then(|()| fs::remove_file(&journal)) {
            // The journal stays naming no step, so whoever opens next
            // deletes it and does nothing else.
            if let Err(emptied) = self.replace_journal(None) {
                diag!(
                    "warning: the write is applied, but {} remains: {e}; it cannot be \
                     emptied either ({emptied}), and the next opener repeats its steps",
                    journal.display()
                );
            }
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

    /// Replace the journal with one of `step` alone, flushed, so whoever
    /// finishes the write makes its history commit and touches no file. A
    /// failure leaves the whole journal, whose file step then runs again.
    fn narrow(&self, step: &HistoryStep) {
        match self.replace_journal(Some(step)) {
            Ok(()) => failpoint::hit(Site::Narrowed),
            Err(e) => diag!(
                "warning: {}: cannot narrow the journal to its history commit: {e}; the next \
                 opener puts the write's files in place again",
                self.journal_path().display()
            ),
        }
    }

    /// Put in place, flushed, a journal of `history` alone, or of no step.
    fn replace_journal(&self, history: Option<&HistoryStep>) -> Result<()> {
        let left = Journal {
            writes: Vec::new(),
            removes: Vec::new(),
            // clone: the journal is written from its own copy of the step.
            history: history.cloned(),
        };
        let state_dir = self.state_dir();
        self.place_journal(&left)
            .and_then(|()| sync_dir(&state_dir).map_err(Error::io(&state_dir)))
    }

    fn apply(&self, _lock: &StoreLock, journal: &Journal) -> Result<()> {
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
        let unapplied = self.unapplied(journal)?;
        self.apply(lock, &unapplied).map_err(|e| self.pending(e))?;
        match self.finish(lock, &unapplied, None) {
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
        Ok(Journal { writes, ..journal })
    }

    /// The bytes of `path`, relative to the store root; `None` when it is gone.
    fn read_if_present(&self, path: &Path) -> Result<Option<Vec<u8>>> {
        read_if_regular(&self.root.join(path))
    }

    /// `path` relative to the store root, as the journal records it.
    fn relative(&self, path: &Path) -> Result<PathBuf> {
        match path.strip_prefix(&self.root) {
            Ok(relative) => Ok(relative.to_path_buf()),
            // The prefix is all the error would say.
            Err(_) => Err(Error::StorePath {
                path: path.to_path_buf(),
                fault: StorePathFault::Outside,
            }),
        }
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
    use super::super::history::HistoryFinding;
    use super::super::message::Tool;
    use super::*;
    use crate::store::Placement;
    use crate::store::testing::{
        changed_in, graph_git, graph_log, sample_component, setup_store, shared_store,
    };
    use tempfile::TempDir;

    /// A journal of every part: a write, a removal, and the history step
    /// with a parent, a blob and a removal.
    const FULL: &str = r#"remove = ["decisions/use-jwt.toml"]

[[write]]
temp = ".state/tmp/4242-0"
target = "components/auth.toml"
blake3 = "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"

[history]
parent = "0123456789abcdef0123456789abcdef01234567"
remove = ["decisions/use-jwt.toml"]

[history.message]
tool = "apply"
origin = "row 1"

[[history.blob]]
path = "components/auth.toml"
id = "89abcdef0123456789abcdef0123456789abcdef"
"#;

    #[test]
    fn a_journal_reads_back_as_it_was_written_with_and_without_its_parts() {
        let full: Journal = toml::from_str(FULL).unwrap();
        assert_eq!(toml::to_string(&full).unwrap(), FULL);
        assert!(!full.writes.is_empty() && !full.removes.is_empty());

        let narrowed = Journal {
            writes: vec![],
            removes: vec![],
            history: full.history.clone(),
        };
        let text = toml::to_string(&narrowed).unwrap();
        assert!(text.starts_with("[history]\n"), "{text}");
        assert_eq!(toml::from_str::<Journal>(&text).unwrap(), narrowed);

        let tracked = Journal {
            history: None,
            ..toml::from_str(FULL).unwrap()
        };
        let text = toml::to_string(&tracked).unwrap();
        assert!(!text.contains("history"), "{text}");
        assert_eq!(toml::from_str::<Journal>(&text).unwrap(), tracked);
    }

    /// A journal names paths and object names that reach the file system
    /// and git: each one that is no plain path below the store, or no
    /// object name, is refused before anything is applied.
    #[test]
    fn a_journal_path_outside_the_store_or_a_forged_object_name_is_refused() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let forged = [
            FULL.replace(
                "remove = [\"decisions/use-jwt.toml\"]\n\n[[write]]",
                "remove = [\"../outside.toml\"]\n\n[[write]]",
            ),
            FULL.replace(
                "target = \"components/auth.toml\"",
                "target = \"/etc/passwd\"",
            ),
            FULL.replace("path = \"components/auth.toml\"", "path = \"\""),
            FULL.replace(
                "parent = \"0123456789abcdef0123456789abcdef01234567\"",
                "parent = \"--output=x\"",
            ),
            FULL.replace("id = \"89ab", "id = \"89AB"),
            FULL.replace("[history]\n", "[history]\nextra = 1\n"),
        ];
        for text in forged {
            fs::write(store.journal_path(), &text).unwrap();

            let err = store
                .replay_journal(&store.lock().unwrap(), PendingHistory::Fail)
                .unwrap_err();

            assert!(matches!(err, Error::BadJournal { .. }), "{text}: {err}");
            assert!(err.to_string().contains("txn.toml"), "{err}");
        }
    }

    /// Stage a component write, as a commit would, and return its journal.
    fn stage_component(store: &Store, name: &str) -> Journal {
        let write = store
            .prepare_write(&store.component_path(name), &sample_component())
            .unwrap();
        store
            .stage_commit(&store.lock().unwrap(), &[write], &[])
            .unwrap()
    }

    #[test]
    fn a_staged_commit_applies_and_leaves_no_journal() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let journal = stage_component(&store, "auth");
        let lock = store.lock().unwrap();

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
        let mut journal = stage_component(&store, "auth");
        journal
            .writes
            .extend(stage_component(&store, "billing").writes);
        let lock = store.lock().unwrap();
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
        let journal = stage_component(&store, "auth");
        let lock = store.lock().unwrap();
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
        let journal = stage_component(&store, "auth");
        let lock = store.lock().unwrap();
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
        let head = store.prepare_history(&lock).unwrap();
        let write = store
            .prepare_write(&store.component_path("auth"), &sample_component())
            .unwrap();
        let mut journal = store.stage_commit(&lock, &[write], &[]).unwrap();
        store
            .attach_history(&lock, &mut journal, &traced(), &head)
            .unwrap();
        store.write_journal(&lock, &journal).unwrap();
        store.apply(&lock, &journal).unwrap();
        journal
    }

    /// The bytes the write of [`stopped_after_its_renames`] put in place.
    fn written() -> String {
        crate::store::format::canonical(&sample_component())
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

    /// The commit landed before the stop, and was reworded, packed and
    /// followed by a commit by hand of a hand edit of the write's file
    /// since: the opener finds it after the step's parent and commits
    /// nothing again.
    #[test]
    fn a_history_commit_made_before_the_stop_is_not_made_again() {
        let tmp = TempDir::new().unwrap();
        let store = shared_store(tmp.path());
        let journal = stopped_after_its_renames(&store);
        let step = journal.history.as_ref().unwrap();
        let lock = store.lock().unwrap();
        store.commit_history(&lock, step, None).unwrap();
        drop(lock);
        graph_git(&store, &["commit", "--quiet", "--amend", "-m", "reworded"]);
        graph_git(&store, &["gc", "--quiet", "--prune=now"]);
        fs::write(store.component_path("auth"), "description = \"by hand\"\n").unwrap();
        graph_git(&store, &["commit", "--quiet", "-am", "by hand"]);

        drop(store.begin_write().unwrap());

        assert!(!store.has_journal().unwrap());
        assert_eq!(graph_log(&store), ["by hand", "reworded", "init"]);
    }

    /// A hand edit of the write's file between the stop and the opener is
    /// not the write's: its commit holds the bytes the write staged, and
    /// the edit is left uncommitted for the next `hand edit` commit.
    #[test]
    fn a_hand_edit_after_the_stop_stays_out_of_the_write_s_commit() {
        let tmp = TempDir::new().unwrap();
        let store = shared_store(tmp.path());
        stopped_after_its_renames(&store);
        fs::write(store.component_path("auth"), "description = \"by hand\"\n").unwrap();

        store.load_state().unwrap();

        assert_eq!(
            graph_git(&store, &["show", "HEAD:components/auth.toml"]),
            written()
        );
        assert_eq!(
            store.history_findings(Placement::Shared).unwrap(),
            [HistoryFinding::Uncommitted {
                graph: store.root().to_path_buf(),
                files: vec![PathBuf::from("components/auth.toml")],
            }]
        );
    }

    /// The write's blobs are gone before the opener runs, pruned by a gc
    /// or with a repository deleted and made again: the commit holds the
    /// file as it is on disk, the first commit of a new repository every
    /// graph file.
    #[test]
    fn a_write_whose_blobs_are_gone_is_committed_as_its_files_are() {
        let tmp = TempDir::new().unwrap();
        let store = shared_store(tmp.path());
        stopped_after_its_renames(&store);
        graph_git(&store, &["gc", "--quiet", "--prune=now"]);

        store.load_state().unwrap();

        assert_eq!(graph_log(&store).len(), 2);
        assert_eq!(
            graph_git(&store, &["show", "HEAD:components/auth.toml"]),
            written()
        );

        let tmp = TempDir::new().unwrap();
        let store = shared_store(tmp.path());
        stopped_after_its_renames(&store);
        fs::remove_dir_all(store.root().join(".git")).unwrap();

        store.load_state().unwrap();

        assert_eq!(
            graph_log(&store),
            ["fmt\n\nOrigin: plan row 7\nSession: s-1"]
        );
        assert_eq!(
            changed_in(&store, "HEAD"),
            ["components/auth.toml", "project.toml"]
        );
        assert_eq!(store.history_findings(Placement::Shared).unwrap(), []);
    }

    /// A replay that puts the files in place and cannot make the history
    /// commit says so, and narrows the journal to that commit; once the
    /// commit can be made, the replay finishes the write.
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
        let left = store.read_journal().unwrap().unwrap();
        assert!(
            left.writes.is_empty() && left.removes.is_empty(),
            "{left:?}"
        );
        assert!(left.history.is_some());
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
