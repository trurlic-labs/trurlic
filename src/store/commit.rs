//! The commit path: staging node files and `graph.toml`, validating the
//! graph, and renaming everything into place with `graph.toml` last.

use std::collections::HashSet;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::{Error, Result};

use super::failpoint::{self, Site};
use super::graph::Severity;
use super::schema::GraphIndex;
use super::state::ProjectState;
use super::{Store, StoreLock};

// ── PendingWrite ─────────────────────────────────────────────────────────────

/// A file write staged for batch commit.
/// Created via [`Store::prepare_write`], executed via [`Store::commit_batch`].
#[must_use = "a pending write must be passed to commit_batch or commit_with_graph"]
pub(crate) struct PendingWrite {
    target: PathBuf,
    content: String,
}

impl PendingWrite {
    /// BLAKE3 hash of the serialized content that will be written.
    #[must_use]
    pub(crate) fn content_hash(&self) -> String {
        super::hash_bytes(self.content.as_bytes())
    }
}

impl Store {
    /// Write `value` to `target` atomically via `.state/tmp/`.
    /// Serializes to TOML, writes to a temp file, validates by deserializing
    /// back from disk, then renames to the final path. Caller **must** hold
    /// a [`StoreLock`].
    pub(crate) fn write_atomic<T: Serialize + DeserializeOwned>(
        &self,
        _lock: &StoreLock,
        target: &Path,
        value: &T,
    ) -> Result<()> {
        self.verify_path(target)?;

        let tmp_dir = self.tmp_dir();
        fs::create_dir_all(&tmp_dir)?;

        let filename = target
            .file_name()
            .ok_or_else(|| Error::Validation("target path has no filename".into()))?;
        let tmp_path = tmp_dir.join(filename);

        let content = toml::to_string_pretty(value)?;

        if let Err(e) = fs::write(&tmp_path, &content) {
            return Err(Error::Io(e));
        }

        // Validate written file by deserializing back — catches partial
        // writes, encoding corruption, and serialization round-trip issues.
        let readback = match fs::read_to_string(&tmp_path) {
            Ok(s) => s,
            Err(e) => {
                let _ = fs::remove_file(&tmp_path);
                return Err(Error::Io(e));
            }
        };
        if let Err(e) = toml::from_str::<T>(&readback) {
            let _ = fs::remove_file(&tmp_path);
            return Err(Error::Validation(format!(
                "write verification failed: written file does not deserialize: {e}"
            )));
        }

        if let Some(parent) = target.parent()
            && let Err(e) = fs::create_dir_all(parent)
        {
            let _ = fs::remove_file(&tmp_path);
            return Err(Error::Io(e));
        }

        if let Err(e) = fs::rename(&tmp_path, target) {
            let _ = fs::remove_file(&tmp_path);
            return Err(Error::Io(e));
        }

        Ok(())
    }

    /// Serialize a value to TOML and verify the round-trip.
    /// Returns a [`PendingWrite`] for use with [`commit_batch`](Self::commit_batch).
    /// The content is deserialized back to `T` at this stage so that type-safe
    /// verification happens while the type is still known; `commit_batch`
    /// then verifies filesystem-level integrity via byte-compare.
    pub(crate) fn prepare_write<T: Serialize + DeserializeOwned>(
        &self,
        target: &Path,
        value: &T,
    ) -> Result<PendingWrite> {
        self.verify_path(target)?;

        let content = toml::to_string_pretty(value)?;
        toml::from_str::<T>(&content).map_err(|e| {
            Error::Validation(format!("serialization round-trip verification failed: {e}"))
        })?;
        Ok(PendingWrite {
            target: target.to_path_buf(),
            content,
        })
    }

    /// Execute a batch of writes and removes as a two-phase commit, and
    /// return the store's generation after it.
    ///
    /// Phase 1: write all content to `.state/tmp/`.
    /// Phase 2: verify each temp file (byte-compare; type-safe check was in `prepare_write`).
    /// Phase 3: raise the generation, then rename all temp files to final
    ///          paths (each atomic on POSIX). If `graph_update` is `Some`, it
    ///          is sorted in place and `graph.toml` is the **last** rename,
    ///          the commit point per the storage spec.
    /// Phase 4: remove old files (best-effort — renames already committed).
    ///
    /// Caller **must** hold a [`StoreLock`].
    pub(crate) fn commit_batch(
        &self,
        lock: &StoreLock,
        writes: Vec<PendingWrite>,
        removes: Vec<PathBuf>,
        graph_update: Option<&mut GraphIndex>,
    ) -> Result<u64> {
        if writes.is_empty() && removes.is_empty() && graph_update.is_none() {
            return self.read_generation();
        }

        // Build the full set of writes: node files first, graph.toml last.
        let mut all_writes = writes;
        let graph_rename = graph_update.is_some().then_some(all_writes.len());

        if let Some(index) = graph_update {
            index.sort();
            all_writes.push(self.prepare_write(&self.graph_path(), &*index)?);
        }

        // Verify all target paths up-front before touching the filesystem.
        for write in &all_writes {
            self.verify_path(&write.target)?;
        }
        for path in &removes {
            self.verify_path(path)?;
        }

        let tmp_dir = self.tmp_dir();
        fs::create_dir_all(&tmp_dir)?;

        // Phase 1: Write all to tmp
        let mut staged: Vec<(PathBuf, PathBuf)> = Vec::with_capacity(all_writes.len());

        for (i, write) in all_writes.iter().enumerate() {
            let filename = write
                .target
                .file_name()
                .ok_or_else(|| Error::Validation("target path has no filename".into()))?;
            let tmp_name = format!("{i}_{}", filename.to_string_lossy());
            let tmp_path = tmp_dir.join(tmp_name);

            if let Err(e) = fs::write(&tmp_path, &write.content) {
                cleanup_tmp_files(&staged);
                return Err(Error::Io(e));
            }
            staged.push((tmp_path, write.target.clone()));
        }

        // Phase 2: Verify write integrity — type-safe deserialization already
        // happened in prepare_write; this byte-compare catches filesystem-level
        // corruption (partial writes, bitflips) on the validated content.
        for (i, (tmp_path, _)) in staged.iter().enumerate() {
            let readback = match fs::read_to_string(tmp_path) {
                Ok(s) => s,
                Err(e) => {
                    cleanup_tmp_files(&staged);
                    return Err(Error::Io(e));
                }
            };
            if readback != all_writes[i].content {
                cleanup_tmp_files(&staged);
                return Err(Error::Validation(
                    "batch write verification failed: content mismatch".into(),
                ));
            }
        }

        // Ensure parent directories exist before renaming.
        for (_, target) in &staged {
            if let Some(parent) = target.parent()
                && let Err(e) = fs::create_dir_all(parent)
            {
                cleanup_tmp_files(&staged);
                return Err(Error::Io(e));
            }
        }

        // Raised before the first rename, so a failure to record it leaves the
        // graph untouched. A crash after it leaves the counter ahead of the
        // graph, which still orders every later load correctly.
        let generation = self
            .raise_generation(lock)
            .inspect_err(|_| cleanup_tmp_files(&staged))?;
        rename_staged(&staged, graph_rename)?;

        // Phase 4: Remove old files.
        //
        // Best-effort: renames (Phase 3) already committed the new state.
        // A remove failure here leaves an orphan file but does NOT roll back
        // the successful writes. Crash recovery and `trurlic check` will
        // surface any resulting inconsistency.
        for path in &removes {
            if let Err(e) = fs::remove_file(path)
                && e.kind() != ErrorKind::NotFound
            {
                eprintln!("warning: failed to remove {}: {e}", path.display());
            }
        }

        Ok(generation)
    }

    /// Validate the full graph derived from `state`, then commit node files
    /// and a normalized `graph.toml` in one atomic transaction.
    ///
    /// This is the primary write path for all graph-mutating operations.
    /// It builds an [`InMemoryGraph`] from the current state, runs all
    /// validation checks, and — only if the graph is error-free — exports
    /// a deterministically sorted index and commits it alongside the
    /// provided node file writes. `graph.toml` is renamed last, serving
    /// as the commit point per the storage spec.
    ///
    /// On success, `state` matches disk: its index is the sorted one just
    /// written, its graph the validated one, its generation the new one.
    /// A `state` loaded before another commit is refused with
    /// [`Error::StaleState`].
    pub(super) fn commit_with_graph(
        &self,
        lock: &StoreLock,
        writes: Vec<PendingWrite>,
        removes: Vec<PathBuf>,
        state: &mut ProjectState,
    ) -> Result<()> {
        self.ensure_current(lock, state)?;

        // Pre-check: duplicate node names in the index would cause silent
        // data loss during InMemoryGraph construction (HashMap overwrite).
        {
            let mut seen = HashSet::with_capacity(state.graph_index.nodes.len());
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
        let issues = graph.validate();
        let errors: Vec<&str> = issues
            .iter()
            .filter(|i| i.severity == Severity::Error)
            .map(|i| i.message.as_str())
            .collect();
        if !errors.is_empty() {
            return Err(Error::GraphIntegrity(errors.join("; ")));
        }
        let mut index = graph.to_index(state.graph_index.rebuilt);
        state.generation = self.commit_batch(lock, writes, removes, Some(&mut index))?;
        state.graph_index = index;
        state.graph = graph;

        Ok(())
    }
}

/// Phase 3 of [`Store::commit_batch`]: rename each staged temp file onto its
/// target, in order. `graph_rename` is the index of `graph.toml`, which is
/// staged last so its rename is the commit point. On a failed rename, the
/// failed and all later temp files are removed; earlier renames stand.
fn rename_staged(staged: &[(PathBuf, PathBuf)], graph_rename: Option<usize>) -> Result<()> {
    failpoint::hit(Site::Staged);
    for (i, (tmp_path, target)) in staged.iter().enumerate() {
        let is_graph = graph_rename == Some(i);
        if is_graph {
            failpoint::hit(Site::NodesRenamed);
        }
        if let Err(e) = fs::rename(tmp_path, target) {
            let _ = fs::remove_file(tmp_path);
            for (remaining, _) in staged.iter().skip(i + 1) {
                let _ = fs::remove_file(remaining);
            }
            return Err(Error::Io(e));
        }
        if is_graph {
            failpoint::hit(Site::GraphRenamed);
        }
    }
    Ok(())
}

fn cleanup_tmp_files(staged: &[(PathBuf, PathBuf)]) {
    for (tmp_path, _) in staged {
        let _ = fs::remove_file(tmp_path);
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::store::STATE_DIR;
    use crate::store::testing::*;
    use tempfile::TempDir;

    // ── atomic write guarantees ──────────────────────────────────────────

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

    // ── commit_batch ─────────────────────────────────────────────────────

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

        store.commit_batch(&lock, writes, vec![], None).unwrap();

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

        store.commit_batch(&lock, writes, removes, None).unwrap();

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

        store.commit_batch(&lock, writes, vec![], None).unwrap();

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
        store.commit_batch(&lock, vec![], removes, None).unwrap();
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
            .commit_batch(&lock, vec![], vec![], Some(&mut index))
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
            .commit_batch(&lock, vec![], vec![], Some(&mut index))
            .unwrap();

        let read_back: GraphIndex =
            toml::from_str(&fs::read_to_string(store.graph_path()).unwrap()).unwrap();
        assert_eq!(read_back.nodes[0].name, "a-node");
        assert_eq!(read_back.nodes[1].name, "z-node");
        assert_eq!(read_back.edges[0].from, "a-node");
        assert_eq!(read_back.edges[1].from, "z-node");
    }

    // ── commit_with_graph ────────────────────────────────────────────────

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
            .commit_with_graph(&lock, vec![write], vec![], &mut state)
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
            .commit_with_graph(&lock, vec![], vec![], &mut state)
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
            .commit_with_graph(&lock, vec![w1, w2], vec![], &mut state)
            .unwrap();

        let index: GraphIndex =
            toml::from_str(&fs::read_to_string(store.graph_path()).unwrap()).unwrap();
        let names: Vec<&str> = index.nodes.iter().map(|n| n.name.as_str()).collect();
        // Should be sorted regardless of insertion order.
        assert_eq!(names[0], "a-comp");
    }
}
