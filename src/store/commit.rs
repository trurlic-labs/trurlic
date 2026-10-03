//! The commit path: validating the graph, then putting node files and
//! `graph.toml` on disk through the journal (see `journal.rs`).

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::{Error, Result};

use super::durable::{place, sync_dir};
use super::failpoint::{self, Site};
use super::schema::GraphIndex;
use super::state::ProjectState;
use super::{Store, StoreLock};

/// A file write staged for batch commit.
/// Created via [`Store::prepare_write`], executed via [`Store::commit_batch`].
#[must_use = "a pending write must be passed to commit_batch or commit_with_graph"]
pub(crate) struct PendingWrite {
    pub(super) target: PathBuf,
    pub(super) content: String,
}

impl PendingWrite {
    /// BLAKE3 hash of the serialized content that will be written.
    #[must_use]
    pub(crate) fn content_hash(&self) -> String {
        super::hash_bytes(self.content.as_bytes())
    }
}

impl Store {
    /// Write `value` to `target` on its own, outside a commit: staged and
    /// flushed in `.state/tmp/`, renamed into place, and the directory
    /// flushed. For `init` and `migrate`, which write before a graph exists
    /// to commit.
    pub(crate) fn write_atomic<T: Serialize + DeserializeOwned>(
        &self,
        _lock: &StoreLock,
        target: &Path,
        value: &T,
    ) -> Result<()> {
        let write = self.prepare_write(target, value)?;
        let parent = target
            .parent()
            .ok_or_else(|| Error::Validation(format!("{} has no parent", target.display())))?;
        let tmp_dir = self.tmp_dir();
        fs::create_dir_all(&tmp_dir).map_err(Error::io(&tmp_dir))?;
        fs::create_dir_all(parent).map_err(Error::io(parent))?;
        let staged = self.stage(write.content.as_bytes())?;
        place(&staged, target)?;
        sync_dir(parent).map_err(Error::io(parent))
    }

    /// Serialize `value` to TOML and parse it back as `T`, so a value that
    /// does not survive the round trip is refused before anything touches
    /// disk.
    pub(crate) fn prepare_write<T: Serialize + DeserializeOwned>(
        &self,
        target: &Path,
        value: &T,
    ) -> Result<PendingWrite> {
        self.verify_path(target)?;

        let content = toml::to_string_pretty(value).map_err(|source| Error::TomlSerialize {
            path: target.to_path_buf(),
            source,
        })?;
        toml::from_str::<T>(&content).map_err(|e| {
            Error::Validation(format!("serialization round-trip verification failed: {e}"))
        })?;
        Ok(PendingWrite {
            target: target.to_path_buf(),
            content,
        })
    }

    /// Commit a batch of writes and removes through the journal, and
    /// return the store's generation after it.
    ///
    /// The writes are staged and flushed, the generation raised, and the
    /// journal put in place, which commits them; then each staged file is
    /// renamed onto its target and the removed files go. If `graph_update`
    /// is `Some`, it is sorted in place and `graph.toml` is the last rename,
    /// so a reader without the lock sees the old index until every node
    /// file is in place. After the journal, a failure is
    /// [`Error::CommitPending`]: the commit lands at the next write.
    pub(crate) fn commit_batch(
        &self,
        lock: &StoreLock,
        writes: Vec<PendingWrite>,
        removes: &[PathBuf],
        graph_update: Option<&mut GraphIndex>,
    ) -> Result<u64> {
        if writes.is_empty() && removes.is_empty() && graph_update.is_none() {
            return self.read_generation();
        }

        let mut all_writes = writes;
        if let Some(index) = graph_update {
            index.sort();
            all_writes.push(self.prepare_write(&self.graph_path(), &*index)?);
        }
        for path in all_writes.iter().map(|write| &write.target).chain(removes) {
            self.verify_path(path)?;
        }

        let journal = self.stage_commit(&all_writes, removes)?;
        // Raised before the journal, so a failure to record it leaves the
        // graph untouched. A crash after it leaves the counter ahead of the
        // graph, which still orders every later load correctly.
        let generation = self
            .raise_generation(lock)
            .inspect_err(|_| self.discard(&journal))?;
        failpoint::hit(Site::Staged);
        self.write_journal(lock, &journal)?;
        failpoint::hit(Site::Journaled);
        self.apply_journal(lock, &journal)?;
        Ok(generation)
    }

    /// Validate the full graph derived from `state`, then commit node files
    /// and a normalized `graph.toml` in one journaled commit.
    ///
    /// This is the write path for every graph mutation. It builds an
    /// [`InMemoryGraph`](super::graph::InMemoryGraph) from `state` and
    /// refuses it when it has an error that `state`'s cached graph, the one
    /// last loaded or committed, does not have. A store that is already
    /// invalid stays writable, and no write adds to what is wrong with it.
    /// The new graph's index is exported sorted and committed with the node
    /// file writes.
    ///
    /// On success, `state` matches disk: its index is the sorted one just
    /// written, its graph the validated one, its generation the new one.
    /// A `state` loaded before another commit is refused with
    /// [`Error::StaleState`].
    pub(super) fn commit_with_graph(
        &self,
        lock: &StoreLock,
        writes: Vec<PendingWrite>,
        removes: &[PathBuf],
        state: &mut ProjectState,
    ) -> Result<()> {
        self.ensure_current(lock, state)?;

        // Pre-check: duplicate node names in the index would cause silent
        // data loss during InMemoryGraph construction: the later entry replaces
        // the earlier one.
        {
            let mut seen = BTreeSet::new();
            for node in &state.graph_index.nodes {
                if !seen.insert(&node.name) {
                    return Err(Error::GraphIntegrity(format!(
                        "duplicate node name `{}` in graph index",
                        node.name
                    )));
                }
            }
        }

        let graph = state.build_graph();
        let introduced = graph.introduced_errors(&state.graph);
        if !introduced.is_empty() {
            let messages: Vec<&str> = introduced.iter().map(|i| i.message.as_str()).collect();
            return Err(Error::GraphIntegrity(messages.join("; ")));
        }
        let mut index = graph.to_index(state.graph_index.rebuilt);
        state.generation = self.commit_batch(lock, writes, removes, Some(&mut index))?;
        state.graph_index = index;
        state.graph = graph;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::store::STATE_DIR;
    use crate::store::testing::*;
    use tempfile::TempDir;

    // atomic write guarantees

    #[test]
    fn atomic_write_leaves_no_tmp_on_success() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let comp = sample_component("auth");
        store
            .write_atomic(&lock, &store.component_path("auth"), &comp)
            .unwrap();

        let tmp_dir = store.root().join(STATE_DIR).join("tmp");
        if tmp_dir.exists() {
            let count: usize = fs::read_dir(&tmp_dir).unwrap().count();
            assert_eq!(count, 0, "temp files should be cleaned after atomic write");
        }
    }

    #[test]
    fn atomic_write_creates_parent_dirs() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join(crate::store::STORE_DIR);
        fs::create_dir_all(root.join(STATE_DIR)).unwrap();
        let store = Store::at(root);
        let lock = store.lock().unwrap();

        let comp = sample_component("auth");
        store
            .write_atomic(&lock, &store.component_path("auth"), &comp)
            .unwrap();

        assert!(store.component_path("auth").exists());
    }

    #[test]
    fn atomic_write_rejects_path_outside_root() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let comp = sample_component("auth");
        let outside = tmp.path().join("outside.toml");
        let err = store.write_atomic(&lock, &outside, &comp).unwrap_err();
        assert!(matches!(err, Error::Validation(_)));
    }

    // commit_batch

    #[test]
    fn commit_batch_writes_multiple_files() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let comp1 = sample_component("auth");
        let comp2 = sample_component("database");

        let writes = vec![
            store
                .prepare_write(&store.component_path("auth"), &comp1)
                .unwrap(),
            store
                .prepare_write(&store.component_path("database"), &comp2)
                .unwrap(),
        ];

        store.commit_batch(&lock, writes, &[], None).unwrap();

        let read1 = store.read_component("auth").unwrap();
        assert_eq!(read1, comp1);
        let read2 = store.read_component("database").unwrap();
        assert_eq!(read2, comp2);
    }

    #[test]
    fn commit_batch_writes_and_removes() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let old = sample_component("old-name");
        store
            .write_atomic(&lock, &store.component_path("old-name"), &old)
            .unwrap();

        let new = sample_component("new-name");
        let writes = vec![
            store
                .prepare_write(&store.component_path("new-name"), &new)
                .unwrap(),
        ];
        let removes = vec![store.component_path("old-name")];

        store.commit_batch(&lock, writes, &removes, None).unwrap();

        assert!(store.component_path("new-name").exists());
        assert!(!store.component_path("old-name").exists());
    }

    #[test]
    fn commit_batch_leaves_no_tmp_files() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let comp = sample_component("auth");
        let writes = vec![
            store
                .prepare_write(&store.component_path("auth"), &comp)
                .unwrap(),
        ];

        store.commit_batch(&lock, writes, &[], None).unwrap();

        let tmp_dir = store.root().join(STATE_DIR).join("tmp");
        if tmp_dir.exists() {
            let count: usize = fs::read_dir(&tmp_dir).unwrap().count();
            assert_eq!(count, 0, "temp files should be cleaned after batch commit");
        }
    }

    #[test]
    fn commit_batch_tolerates_already_removed_file() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let removes = vec![store.component_path("nonexistent")];
        store.commit_batch(&lock, vec![], &removes, None).unwrap();
    }

    #[test]
    fn commit_batch_writes_graph_update() {
        use crate::store::schema::*;

        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let mut index = GraphIndex {
            version: 1,
            rebuilt: None,
            nodes: vec![NodeEntry {
                name: "test".into(),
                kind: NodeKind::Component,
                tags: vec![],
                hash: "abc".into(),
            }],
            edges: vec![],
        };

        store
            .commit_batch(&lock, vec![], &[], Some(&mut index))
            .unwrap();

        assert!(store.graph_path().exists());
        let read_back: GraphIndex =
            toml::from_str(&fs::read_to_string(store.graph_path()).unwrap()).unwrap();
        assert_eq!(read_back.nodes.len(), 1);
        assert_eq!(read_back.nodes[0].name, "test");
    }

    #[test]
    fn commit_batch_sorts_graph_index() {
        use crate::store::schema::*;

        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        // Deliberately unsorted nodes and edges.
        let mut index = GraphIndex {
            version: 1,
            rebuilt: None,
            nodes: vec![
                NodeEntry {
                    name: "z-node".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: "z".into(),
                },
                NodeEntry {
                    name: "a-node".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: "a".into(),
                },
            ],
            edges: vec![
                EdgeEntry {
                    from: "z-node".into(),
                    to: "a-node".into(),
                    kind: EdgeKind::ConnectsTo,
                },
                EdgeEntry {
                    from: "a-node".into(),
                    to: "z-node".into(),
                    kind: EdgeKind::BelongsTo,
                },
            ],
        };

        store
            .commit_batch(&lock, vec![], &[], Some(&mut index))
            .unwrap();

        let read_back: GraphIndex =
            toml::from_str(&fs::read_to_string(store.graph_path()).unwrap()).unwrap();
        assert_eq!(read_back.nodes[0].name, "a-node");
        assert_eq!(read_back.nodes[1].name, "z-node");
        assert_eq!(read_back.edges[0].from, "a-node");
        assert_eq!(read_back.edges[1].from, "z-node");
    }

    // commit_with_graph

    #[test]
    fn commit_with_graph_validates_and_writes() {
        use crate::store::schema::*;

        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let comp = sample_component("auth");
        let write = store
            .prepare_write(&store.component_path("auth"), &comp)
            .unwrap();
        let hash = write.content_hash();

        let mut state = store.load_state().unwrap();
        state.graph_index.nodes.push(NodeEntry {
            name: "auth".into(),
            kind: NodeKind::Component,
            tags: vec![],
            hash,
        });
        state.components.insert("auth".into(), Arc::new(comp));

        store
            .commit_with_graph(&lock, vec![write], &[], &mut state)
            .unwrap();

        assert!(store.component_path("auth").exists());

        let index: GraphIndex =
            toml::from_str(&fs::read_to_string(store.graph_path()).unwrap()).unwrap();
        assert!(index.nodes.iter().any(|n| n.name == "auth"));
    }

    #[test]
    fn commit_with_graph_rejects_invalid_graph() {
        use crate::store::schema::*;

        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let mut state = store.load_state().unwrap();
        state.graph_index.nodes.push(NodeEntry {
            name: "orphan".into(),
            kind: NodeKind::Decision,
            tags: vec![],
            hash: "fake".into(),
        });
        state.graph_index.edges.push(EdgeEntry {
            from: "orphan".into(),
            to: "nonexistent".into(),
            kind: EdgeKind::BelongsTo,
        });
        state.decisions.insert(
            "orphan".into(),
            sample_decision("orphan", "nonexistent").into(),
        );

        let err = store
            .commit_with_graph(&lock, vec![], &[], &mut state)
            .unwrap_err();
        assert!(matches!(err, Error::GraphIntegrity(_)));
    }

    /// A commit that restamped the index would change `graph.toml` even when
    /// the graph did not, and conflict on every concurrent branch.
    #[test]
    fn commits_keep_the_index_stamp() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        fs::remove_file(store.graph_path()).unwrap();
        let lock = store.lock().unwrap();
        let mut state = store.load_state().unwrap();
        let stamp = state.graph_index.rebuilt;
        assert!(stamp.is_some(), "a repaired index is stamped once");

        store.add_component(&lock, &mut state, "auth", "").unwrap();
        store
            .add_component(&lock, &mut state, "billing", "")
            .unwrap();

        let on_disk: GraphIndex =
            toml::from_str(&fs::read_to_string(store.graph_path()).unwrap()).unwrap();
        assert_eq!(on_disk.rebuilt, stamp);
    }

    /// Readers serialize `state.graph_index` (the map's edge list), so after a
    /// commit it must be the sorted index on disk, not the append order.
    #[test]
    fn a_commit_leaves_the_in_memory_index_equal_to_disk() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();
        let mut state = store.load_state().unwrap();

        store.add_component(&lock, &mut state, "zeta", "").unwrap();
        store.add_component(&lock, &mut state, "alpha", "").unwrap();
        store
            .add_connection(&lock, &mut state, "zeta", "alpha")
            .unwrap();

        assert_eq!(state.graph_index, store.load_state().unwrap().graph_index);
    }

    #[test]
    fn commit_with_graph_normalizes_index() {
        use crate::store::schema::*;

        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let c1 = sample_component("z-comp");
        let w1 = store
            .prepare_write(&store.component_path("z-comp"), &c1)
            .unwrap();
        let c2 = sample_component("a-comp");
        let w2 = store
            .prepare_write(&store.component_path("a-comp"), &c2)
            .unwrap();

        let mut state = store.load_state().unwrap();
        // Push in reverse-alphabetical order.
        state.graph_index.nodes.push(NodeEntry {
            name: "z-comp".into(),
            kind: NodeKind::Component,
            tags: vec![],
            hash: w1.content_hash(),
        });
        state.graph_index.nodes.push(NodeEntry {
            name: "a-comp".into(),
            kind: NodeKind::Component,
            tags: vec![],
            hash: w2.content_hash(),
        });
        state.graph_index.edges.push(EdgeEntry {
            from: "z-comp".into(),
            to: "a-comp".into(),
            kind: EdgeKind::ConnectsTo,
        });
        state.components.insert("z-comp".into(), Arc::new(c1));
        state.components.insert("a-comp".into(), Arc::new(c2));

        store
            .commit_with_graph(&lock, vec![w1, w2], &[], &mut state)
            .unwrap();

        let index: GraphIndex =
            toml::from_str(&fs::read_to_string(store.graph_path()).unwrap()).unwrap();
        let names: Vec<&str> = index.nodes.iter().map(|n| n.name.as_str()).collect();
        // Should be sorted regardless of insertion order.
        assert_eq!(names[0], "a-comp");
    }
}
