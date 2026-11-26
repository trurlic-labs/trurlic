//! The two ways into the graph: a read of the files on disk, and a
//! [`Writer`] that commits to them.
//!
//! No process keeps the graph between calls. A read loads it under the
//! shared lock; a write loads it under the exclusive lock and validates
//! against that load, which the writer owns with the lock, so no write can
//! be built on a graph loaded before another commit. Either one first
//! finishes what an interrupted commit left, so neither sees half of one;
//! a read goes on when only that write's history commit cannot be made,
//! and leaves it to the next writer.

use std::mem;
use std::path::PathBuf;

use crate::Result;

use super::commit::PendingWrite;
use super::failpoint::{self, Site};
use super::history::History;
use super::journal::{PendingHistory, Replayed};
use super::lock::StoreLock;
use super::message::{CommitMessage, Tool};
use crate::store::Store;
use crate::store::model::NodeFile;
use crate::store::model::ProjectState;
use crate::store::model::{Placement, Project};

impl Store {
    /// The graph as the files on disk hold it, loaded under the shared lock,
    /// or under the exclusive lock when an interrupted write must be
    /// finished first.
    pub fn load_state(&self) -> Result<ProjectState> {
        self.read(|project, bytes| {
            let state = self.read_graph(project, &bytes)?;
            failpoint::panic(Site::Loaded);
            Ok(state)
        })
    }

    /// `project.toml` as a read finds it: the graph's settings and its
    /// `[quality]`, none of its nodes.
    pub(in crate::store) fn load_project(&self) -> Result<Project> {
        self.read(|project, _| Ok(project))
    }

    /// Take the exclusive lock, finish what an interrupted write left, and
    /// load the graph under the lock. Every write to a current-format graph
    /// starts here and commits through the returned [`Writer`]; `init` and
    /// `migrate`, which write before one exists, take the lock themselves.
    pub fn begin_write(&self) -> Result<Writer<'_>> {
        let lock = self.lock()?;
        let (project, bytes) = self.recover_current(&lock, PendingHistory::Fail)?;
        let state = self.read_graph(project, &bytes)?;
        failpoint::panic(Site::Loaded);
        Ok(Writer {
            store: self,
            lock,
            state,
        })
    }

    /// Run `load` on `project.toml` and its bytes while no commit can land,
    /// once the format is checked: a store in another format is refused
    /// before anything else in it is read.
    ///
    /// `load` runs under the shared lock. Under it no commit is in flight,
    /// so a journal or a temp file found there was left by a writer that
    /// stopped. `load` then runs under the exclusive lock, after that commit
    /// is finished: reading around it would see a graph halfway between two
    /// commits.
    fn read<T>(&self, load: impl FnOnce(Project, Vec<u8>) -> Result<T>) -> Result<T> {
        {
            let _shared = self.lock_shared()?;
            let (project, bytes) = self.read_current_project()?;
            if !self.has_leftovers()? {
                return load(project, bytes);
            }
        }
        let lock = self.lock()?;
        let (project, bytes) = self.recover_current(&lock, PendingHistory::Keep)?;
        load(project, bytes)
    }

    /// Check the format, then finish what an interrupted write left, so
    /// nothing in a store of another format is touched. `project.toml` is
    /// read once, and again when a journal was replayed: a journal can write
    /// `project.toml`.
    fn recover_current(
        &self,
        lock: &StoreLock,
        pending: PendingHistory,
    ) -> Result<(Project, Vec<u8>)> {
        let (table, bytes) = self.read_current_table()?;
        match self.recover_with(lock, pending)? {
            Replayed::Nothing => Ok((self.project_of(table)?, bytes)),
            Replayed::Finished | Replayed::HistoryWaits => self.read_current_project(),
        }
    }
}

/// The exclusive lock and the graph loaded under it. Every write to a
/// current-format graph is a method of a writer, so a write validates against the store as it is
/// while the lock is held, and each commit leaves the writer's graph equal
/// to the files on disk. Dropping the writer releases the lock.
#[must_use = "dropping the writer releases the lock"]
pub struct Writer<'s> {
    pub(in crate::store) store: &'s Store,
    lock: StoreLock,
    pub(in crate::store) state: ProjectState,
}

impl Writer<'_> {
    /// The graph as the last commit, or the load, left it on disk.
    #[must_use]
    pub fn state(&self) -> &ProjectState {
        &self.state
    }

    /// The store it writes, for what a write reads beside the graph.
    #[must_use]
    pub const fn store(&self) -> &Store {
        self.store
    }

    /// The graph as the last commit left it on disk, the lock released: a
    /// read of it that holds no other reader or writer up.
    #[must_use]
    pub fn into_state(self) -> ProjectState {
        self.state
    }

    /// Rewrite each file whose bytes are not the canonical form of what it
    /// holds, in one commit, and return those files. The graph does not
    /// change: every write reads back as the value it was made from, so it
    /// is validated by construction, and no other file is touched. Under
    /// `shared` it leaves the history settled too: hand edits are committed
    /// as `hand edit` when no file needs a rewrite, as before any write.
    pub fn format(&mut self) -> Result<Vec<NodeFile>> {
        let writes = self
            .state
            .non_canonical
            .iter()
            .filter_map(|file| self.rewrite(file).transpose())
            .collect::<Result<Vec<_>>>()?;
        if writes.is_empty() && self.state.project.placement == Placement::Shared {
            self.store.prepare_history(&self.lock)?;
        }
        self.commit(&writes, &[], &CommitMessage::of(Tool::Fmt))?;
        Ok(mem::take(&mut self.state.non_canonical))
    }

    /// Commit `writes` and `removes` in one journaled commit, with the
    /// history the graph's placement keeps: under `shared`, one commit to
    /// the graph's repository with `message`, after one of any hand edit.
    /// The caller has applied the write to the state, which the commit
    /// leaves equal to the files on disk.
    pub(in crate::store) fn commit(
        &self,
        writes: &[PendingWrite],
        removes: &[PathBuf],
        message: &CommitMessage,
    ) -> Result<()> {
        let history = History::of(self.state.project.placement, message);
        self.store
            .commit_batch(&self.lock, writes, removes, history)
    }

    /// The canonical text of `file`. Every listed file was read into the
    /// state with it, so a node it names is always there.
    fn rewrite(&self, file: &NodeFile) -> Result<Option<PendingWrite>> {
        let Self { store, state, .. } = self;
        let path = store.path_of(file);
        match file {
            NodeFile::Project => store.prepare_write(&path, &state.project).map(Some),
            NodeFile::Component(name) => state
                .components
                .get(name)
                .map(|component| store.prepare_write(&path, component))
                .transpose(),
            NodeFile::Decision(name) => state
                .decisions
                .get(name)
                .map(|decision| store.prepare_write(&path, decision))
                .transpose(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::Error;
    use crate::store::format::canonical;
    use crate::store::testing::{
        sample_component, sample_decision, setup_store, setup_store_with_version, write_node,
    };
    use tempfile::TempDir;

    /// `decisions/d.toml` holding the sample decision with its keys out of
    /// order: a hand edit that loads.
    fn hand_edit(store: &Store) -> String {
        let canonical = canonical(&sample_decision("project"));
        let (first, rest) = canonical.split_once('\n').unwrap();
        let edited = format!("{rest}{first}\n");
        fs::write(store.decision_path("d"), &edited).unwrap();
        edited
    }

    #[test]
    fn format_rewrites_exactly_the_non_canonical_files() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        write_node(
            &store,
            &NodeFile::Component("auth".into()),
            &sample_component(),
        );
        let untouched = fs::metadata(store.component_path("auth"))
            .unwrap()
            .modified()
            .unwrap();
        hand_edit(&store);

        let formatted = store.begin_write().unwrap().format().unwrap();

        assert_eq!(formatted, [NodeFile::Decision("d".into())]);
        assert_eq!(
            fs::read_to_string(store.decision_path("d")).unwrap(),
            canonical(&sample_decision("project"))
        );
        let modified = fs::metadata(store.component_path("auth"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(modified, untouched);
        let state = store.load_state().unwrap();
        assert_eq!(state.non_canonical, []);
        assert_eq!(state.decisions["d"], sample_decision("project"));
        assert_eq!(store.begin_write().unwrap().format().unwrap(), []);
    }

    /// A file under the cap whose canonical form is over it, a literal
    /// string of backslashes that the canonical form escapes: it loads,
    /// and `fmt` writes nothing and names it, its size and the cap.
    #[test]
    fn format_refuses_a_rewrite_a_load_could_not_read() {
        use crate::store::limits::MAX_NODE_FILE_BYTES;

        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let canonical = canonical(&sample_decision("project"));
        let reason = "\\".repeat(MAX_NODE_FILE_BYTES / 2 + 1);
        let text = canonical.replace(
            "reason = \"Reason of the sample decision\"",
            &format!("reason = '{reason}'"),
        );
        assert!(text.len() < MAX_NODE_FILE_BYTES);
        fs::write(store.decision_path("d"), &text).unwrap();
        let state = store.load_state().unwrap();
        assert_eq!(state.non_canonical, [NodeFile::Decision("d".into())]);

        let err = store.begin_write().unwrap().format().unwrap_err();

        assert!(
            matches!(&err, Error::CanonicalTooLarge { path, bytes, max: MAX_NODE_FILE_BYTES }
                if *path == store.decision_path("d") && *bytes > MAX_NODE_FILE_BYTES),
            "{err:?}"
        );
        assert_eq!(fs::read_to_string(store.decision_path("d")).unwrap(), text);
    }

    #[test]
    fn format_rewrites_project_toml_too() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let path = store.project_path();
        let text = fs::read_to_string(&path).unwrap();
        fs::write(&path, text.replace(" = ", "=")).unwrap();

        let formatted = store.begin_write().unwrap().format().unwrap();

        assert_eq!(formatted, [NodeFile::Project]);
        assert_eq!(fs::read_to_string(&path).unwrap(), text);
    }

    #[test]
    fn a_writer_loads_what_an_earlier_writer_committed() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        hand_edit(&store);
        store.begin_write().unwrap().format().unwrap();

        let writer = store.begin_write().unwrap();

        assert_eq!(writer.state().non_canonical, []);
    }

    #[test]
    fn a_load_finishes_a_commit_a_stopped_writer_journaled() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();
        let write = store
            .prepare_write(&store.component_path("auth"), &sample_component())
            .unwrap();
        let journal = store.stage_commit(&lock, &[write], &[]).unwrap();
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
        let edited = hand_edit(&store);
        let mut writer = store.begin_write().unwrap();

        std::thread::scope(|scope| {
            let reader = scope.spawn(|| store.load_state());
            writer.format().unwrap();
            drop(writer);
            let state = reader.join().unwrap().unwrap();
            assert_eq!(state.non_canonical, [], "the load saw {edited:?}");
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
        let store = setup_store_with_version(tmp.path(), "0.4.1");

        let err = store.begin_write().map(|_| ()).unwrap_err();
        assert!(matches!(err, Error::VersionMismatch { .. }), "{err}");
        let err = store.load_state().map(|_| ()).unwrap_err();
        assert!(matches!(err, Error::VersionMismatch { .. }), "{err}");
    }
}
