//! The commit path: a file's canonical text, read back before anything
//! touches disk, then put in place through the journal (see `journal.rs`)
//! and, under `shared`, committed to the graph's history (`history.rs`).

use std::fs;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;

use crate::{Error, Result};

use super::durable::{place, sync_dir};
use super::failpoint::{self, Site};
use super::history::History;
use super::lock::StoreLock;
use crate::store::format::{Canonical, canonical};
use crate::store::limits::MAX_NODE_FILE_BYTES;
use crate::store::{Store, StorePathFault};

/// A file write staged for batch commit.
/// Created via [`Store::prepare_write`], executed via [`Store::commit_batch`].
#[must_use = "a pending write must be passed to commit_batch"]
pub(crate) struct PendingWrite {
    pub(super) target: PathBuf,
    pub(super) content: String,
}

impl Store {
    /// Write `value` to `target` on its own, outside a commit: staged and
    /// flushed in `.state/tmp/`, renamed into place, and the directory
    /// flushed. For `init`, which writes before a graph exists to commit.
    pub(super) fn write_atomic<T: Canonical + DeserializeOwned + PartialEq>(
        &self,
        _lock: &StoreLock,
        target: &Path,
        value: &T,
    ) -> Result<()> {
        let write = self.prepare_write(target, value)?;
        let parent = target.parent().ok_or_else(|| Error::StorePath {
            path: target.to_path_buf(),
            fault: StorePathFault::NoParent,
        })?;
        let tmp_dir = self.tmp_dir();
        fs::create_dir_all(&tmp_dir).map_err(Error::io(&tmp_dir))?;
        fs::create_dir_all(parent).map_err(Error::io(parent))?;
        let staged = self.stage(write.content.as_bytes())?;
        place(&staged, target)?;
        sync_dir(parent).map_err(Error::io(parent))
    }

    /// The canonical text of `value` for `target`, refused before anything
    /// touches disk unless a load reads it back as `value`: within
    /// [`MAX_NODE_FILE_BYTES`] ([`Error::CanonicalTooLarge`]), as TOML of
    /// its type ([`Error::CanonicalUnreadable`]), and as the value written
    /// ([`Error::CanonicalDrift`]). Every write of a graph file passes here.
    pub(in crate::store) fn prepare_write<T: Canonical + DeserializeOwned + PartialEq>(
        &self,
        target: &Path,
        value: &T,
    ) -> Result<PendingWrite> {
        self.verify_path(target)?;
        let content = canonical(value);
        if content.len() > MAX_NODE_FILE_BYTES {
            return Err(Error::CanonicalTooLarge {
                path: target.to_path_buf(),
                bytes: content.len(),
                max: MAX_NODE_FILE_BYTES,
            });
        }
        let read_back =
            toml::from_str::<T>(&content).map_err(|source| Error::CanonicalUnreadable {
                path: target.to_path_buf(),
                source,
            })?;
        if read_back != *value {
            return Err(Error::CanonicalDrift {
                path: target.to_path_buf(),
            });
        }
        Ok(PendingWrite {
            target: target.to_path_buf(),
            content,
        })
    }

    /// Commit a batch of writes and removes through the journal, with its
    /// history.
    ///
    /// Under [`History::Graph`] the history step runs first: graph files a
    /// hand edit left uncommitted are committed as `hand edit`, and a write
    /// the graph's repository cannot take is refused with nothing written.
    /// Then the writes are staged and flushed, under `shared` written to
    /// the graph's repository as blobs too, and the journal, carrying the
    /// history step, put in place, which commits them; each staged file is
    /// renamed onto its target, the removed files go, and the history
    /// commit is made from the blobs. After the journal, a failure is
    /// [`Error::WritePending`]: the write lands at the next read or write.
    pub(in crate::store) fn commit_batch(
        &self,
        lock: &StoreLock,
        writes: &[PendingWrite],
        removes: &[PathBuf],
        history: History<'_>,
    ) -> Result<()> {
        if writes.is_empty() && removes.is_empty() {
            return Ok(());
        }
        for path in writes.iter().map(|write| &write.target).chain(removes) {
            self.verify_path(path)?;
        }
        let head = match history {
            History::Project => None,
            History::Graph(message) => Some((message, self.prepare_history(lock)?)),
        };

        let mut journal = self.stage_commit(lock, writes, removes)?;
        failpoint::hit(Site::Staged);
        let head = match head {
            Some((message, head)) => {
                self.attach_history(lock, &mut journal, message, &head)?;
                failpoint::hit(Site::Blobs);
                Some(head)
            }
            None => None,
        };
        self.write_journal(lock, &journal)?;
        failpoint::hit(Site::Journaled);
        self.apply_journal(lock, &journal, head)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::STATE_DIR;
    use crate::store::model::Component;
    use crate::store::testing::*;
    use tempfile::TempDir;

    fn read_component(store: &Store, name: &str) -> Component {
        toml::from_str(&fs::read_to_string(store.component_path(name)).unwrap()).unwrap()
    }

    #[test]
    fn atomic_write_leaves_no_tmp_and_creates_parent_dirs() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join(crate::store::STORE_DIR);
        fs::create_dir_all(root.join(STATE_DIR)).unwrap();
        let store = Store::at(root);
        let lock = store.lock().unwrap();

        store
            .write_atomic(&lock, &store.component_path("auth"), &sample_component())
            .unwrap();

        assert_eq!(read_component(&store, "auth"), sample_component());
        assert_eq!(fs::read_dir(store.tmp_dir()).unwrap().count(), 0);
    }

    #[test]
    fn a_write_outside_the_store_is_refused() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let outside = tmp.path().join("outside.toml");
        let err = store
            .write_atomic(&lock, &outside, &sample_component())
            .unwrap_err();
        assert!(
            matches!(&err, Error::StorePath { path, fault: StorePathFault::Outside } if *path == outside),
            "{err:?}"
        );
    }

    #[test]
    fn a_prepared_write_is_the_canonical_text() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let write = store
            .prepare_write(&store.decision_path("d"), &sample_decision("project"))
            .unwrap();
        assert_eq!(write.content, canonical(&sample_decision("project")));
    }

    #[test]
    fn commit_batch_writes_and_removes_in_one_commit() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();
        store
            .write_atomic(
                &lock,
                &store.component_path("old-name"),
                &sample_component(),
            )
            .unwrap();
        let mut renamed = sample_component();
        renamed.description = "Renamed".into();

        let writes = vec![
            store
                .prepare_write(&store.component_path("new-name"), &renamed)
                .unwrap(),
        ];
        let removes = vec![
            store.component_path("old-name"),
            store.component_path("never-there"),
        ];
        store
            .commit_batch(&lock, &writes, &removes, History::Project)
            .unwrap();

        assert_eq!(read_component(&store, "new-name"), renamed);
        assert!(!store.component_path("old-name").exists());
        assert_eq!(fs::read_dir(store.tmp_dir()).unwrap().count(), 0);
    }
}
