//! Loading the graph from its files: `project.toml`, every node file and
//! `graph.toml`, then the index reconciled with the node files in memory.
//!
//! Nothing here takes a lock. Every load runs inside `access`, which holds
//! the shared or the exclusive lock around it, so a load never reads a
//! commit halfway through its renames.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rayon::prelude::*;
use serde::de::DeserializeOwned;

use crate::{Error, Result};

use super::schema::{EdgeEntry, EdgeKind, GraphIndex, NodeEntry, NodeKind};
use super::{
    ComponentFile, DecisionFile, PatternFile, ProjectFile, ProjectState, Store, Timestamp,
};

/// A node file as read: its name, its contents, and the BLAKE3 of its bytes.
pub(super) type NodeFile<T> = (String, T, String);

/// Every node file of the store, each kind sorted by name.
pub(super) struct NodeFiles {
    pub(super) components: Vec<NodeFile<ComponentFile>>,
    pub(super) decisions: Vec<NodeFile<DecisionFile>>,
    pub(super) patterns: Vec<NodeFile<PatternFile>>,
}

impl Store {
    /// Read a TOML file and hash its bytes in one read. The hash covers the
    /// bytes on disk, not a re-serialization, so a hand edit changes it.
    pub(super) fn read_toml_with_hash<T: DeserializeOwned>(
        &self,
        path: &Path,
    ) -> Result<(T, String)> {
        let bytes = fs::read(path).map_err(Error::io(path))?;
        let hash = blake3::hash(&bytes).to_hex().to_string();
        let parsed = toml::from_slice(&bytes).map_err(Error::toml(path))?;
        Ok((parsed, hash))
    }

    /// Every node file and `graph.toml`, reconciled into one state.
    ///
    /// `graph.toml` is one large file parsed on one thread, so it is parsed
    /// while the other threads parse the node files.
    pub(super) fn read_graph(
        &self,
        project: ProjectFile,
        project_hash: String,
    ) -> Result<ProjectState> {
        let (files, existing) = rayon::join(|| self.read_node_files(), || self.read_index());
        let (index, components, decisions, patterns) = reconcile(existing?, project_hash, files?);
        let mut state = ProjectState::new(project, components, decisions, patterns, index);
        // code_refs are relative to the project directory, which is the parent
        // of `.trurlic/`. Orphaned-ref detection resolves them against this root.
        state.project_root = self.root.parent().unwrap_or(&self.root).to_path_buf();
        Ok(state)
    }

    /// The three kinds are read concurrently: listing a directory and
    /// waiting for its last file are serial steps of each.
    fn read_node_files(&self) -> Result<NodeFiles> {
        let (decisions, (components, patterns)) = rayon::join(
            || self.read_nodes(self.list_decisions()?, Self::decision_path),
            || {
                rayon::join(
                    || self.read_nodes(self.list_components()?, Self::component_path),
                    || self.read_nodes(self.list_patterns()?, Self::pattern_path),
                )
            },
        );
        Ok(NodeFiles {
            components: components?,
            decisions: decisions?,
            patterns: patterns?,
        })
    }

    /// Read and hash the named node files in parallel, each in one read.
    fn read_nodes<T: DeserializeOwned + Send>(
        &self,
        names: Vec<String>,
        path: fn(&Self, &str) -> PathBuf,
    ) -> Result<Vec<NodeFile<T>>> {
        names
            .into_par_iter()
            .map(|name| {
                let (file, hash) = self.read_toml_with_hash::<T>(&path(self, &name))?;
                Ok((name, file, hash))
            })
            .collect()
    }

    /// `graph.toml`, or an empty index stamped now when there is none.
    fn read_index(&self) -> Result<GraphIndex> {
        let path = self.graph_path();
        match fs::read(&path) {
            Ok(bytes) => toml::from_slice(&bytes).map_err(Error::toml(&path)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(GraphIndex {
                version: 1,
                rebuilt: Some(Timestamp::now()?),
                nodes: vec![],
                edges: vec![],
            }),
            Err(source) => Err(Error::Io { path, source }),
        }
    }
}

pub(super) type Reconciled = (
    GraphIndex,
    BTreeMap<String, Arc<ComponentFile>>,
    BTreeMap<String, Arc<DecisionFile>>,
    BTreeMap<String, Arc<PatternFile>>,
);

/// The index the node files imply, with what only `graph.toml` holds kept.
///
/// The nodes are the node files, each with the hash of its bytes. A
/// decision's tags and its `belongs_to` edge come from its file, so they
/// survive a missing or stale `graph.toml`; a component's or a pattern's
/// tags, and every other edge between existing nodes, come from
/// `graph.toml`.
pub(super) fn reconcile(
    existing: GraphIndex,
    project_hash: String,
    files: NodeFiles,
) -> Reconciled {
    let (nodes, components, decisions, patterns) = index_files(existing.nodes, project_hash, files);
    let exists = |name: &str| {
        name == "project"
            || components.contains_key(name)
            || decisions.contains_key(name)
            || patterns.contains_key(name)
    };
    let mut index = GraphIndex {
        version: 1,
        rebuilt: existing.rebuilt,
        edges: index_edges(existing.edges, &decisions, exists),
        nodes,
    };
    index.sort();
    (index, components, decisions, patterns)
}

type IndexedFiles = (
    Vec<NodeEntry>,
    BTreeMap<String, Arc<ComponentFile>>,
    BTreeMap<String, Arc<DecisionFile>>,
    BTreeMap<String, Arc<PatternFile>>,
);

/// An index node for `project` and for each node file, with the tags of
/// `existing` for every node but a decision, and the files by name.
fn index_files(existing: Vec<NodeEntry>, project_hash: String, files: NodeFiles) -> IndexedFiles {
    let mut existing_tags: BTreeMap<String, Vec<String>> = existing
        .into_iter()
        .map(|node| (node.name, node.tags))
        .collect();
    let mut tags_of = |name: &str| existing_tags.remove(name).unwrap_or_default();

    let node_count = 1 + files.components.len() + files.decisions.len() + files.patterns.len();
    let mut nodes = Vec::with_capacity(node_count);
    nodes.push(NodeEntry {
        name: "project".into(),
        kind: NodeKind::Component,
        tags: tags_of("project"),
        hash: project_hash,
    });
    let components = index_nodes(
        files.components,
        NodeKind::Component,
        &mut nodes,
        |name, _| tags_of(name),
    );
    // clone: the tags live both in the file and in the index node.
    let decision_tags = |_: &str, file: &DecisionFile| file.decision.tags.clone();
    let decisions = index_nodes(
        files.decisions,
        NodeKind::Decision,
        &mut nodes,
        decision_tags,
    );
    let patterns = index_nodes(files.patterns, NodeKind::Pattern, &mut nodes, |name, _| {
        tags_of(name)
    });
    (nodes, components, decisions, patterns)
}

/// The edges of `graph.toml` between nodes that `exists` names, except
/// `belongs_to`, which each decision's file states. A decision whose
/// component does not exist gets no edge; validation reports it.
fn index_edges(
    existing: Vec<EdgeEntry>,
    decisions: &BTreeMap<String, Arc<DecisionFile>>,
    exists: impl Fn(&str) -> bool,
) -> Vec<EdgeEntry> {
    let mut edges: Vec<EdgeEntry> = existing
        .into_iter()
        .filter(|e| e.kind != EdgeKind::BelongsTo && exists(&e.from) && exists(&e.to))
        .collect();
    edges.extend(
        decisions
            .iter()
            .filter(|(_, dec)| exists(&dec.decision.component))
            .map(|(name, dec)| EdgeEntry {
                // clone: the edge names both ends; the decision keeps both.
                from: name.clone(),
                to: dec.decision.component.clone(),
                kind: EdgeKind::BelongsTo,
            }),
    );
    edges
}

/// Push an index node for each file and return the files by name. `files`
/// is sorted by name, so the map is built in one pass.
fn index_nodes<T>(
    files: Vec<NodeFile<T>>,
    kind: NodeKind,
    nodes: &mut Vec<NodeEntry>,
    mut tags: impl FnMut(&str, &T) -> Vec<String>,
) -> BTreeMap<String, Arc<T>> {
    files
        .into_iter()
        .map(|(name, file, hash)| {
            nodes.push(NodeEntry {
                // clone: the name keys the state and names the index node.
                name: name.clone(),
                kind,
                tags: tags(&name, &file),
                hash,
            });
            (name, Arc::new(file))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::hash_file;
    use crate::store::testing::*;
    use tempfile::TempDir;

    #[test]
    fn load_state_reads_everything() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let auth = sample_component("auth");
        store
            .write_atomic(&lock, &store.component_path("auth"), &auth)
            .unwrap();

        let dec = sample_decision("token-format", "auth");
        store
            .write_atomic(&lock, &store.decision_path("token-format"), &dec)
            .unwrap();

        drop(lock);
        let state = store.load_state().unwrap();
        assert_eq!(state.components.len(), 1);
        assert_eq!(state.decisions.len(), 1);
        assert!(state.components.contains_key("auth"));
        assert!(state.decisions.contains_key("token-format"));
        assert!(state.patterns.is_empty());

        // Graph index should have: project + auth + token-format = 3 nodes
        assert_eq!(state.graph_index.nodes.len(), 3);
        // BelongsTo edge inferred for token-format → auth
        assert!(
            state
                .graph_index
                .edges
                .iter()
                .any(|e| e.from == "token-format"
                    && e.to == "auth"
                    && e.kind == EdgeKind::BelongsTo)
        );
    }

    #[test]
    fn load_state_names_a_node_file_that_does_not_parse() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let broken = store.decision_path("broken");
        fs::write(&broken, "[decision]\nchoice = [\n").unwrap();

        let err = store.load_state().map(|_| ()).unwrap_err();

        assert!(
            matches!(&err, Error::Toml { path, .. } if *path == broken),
            "{err:?}"
        );
        assert!(err.to_string().starts_with(&broken.display().to_string()));
    }

    #[test]
    fn load_state_names_a_node_file_that_cannot_be_read() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let unreadable = store.decision_path("unreadable");
        fs::create_dir(&unreadable).unwrap();

        let err = store.load_state().map(|_| ()).unwrap_err();

        assert!(
            matches!(&err, Error::Io { path, .. } if *path == unreadable),
            "{err:?}"
        );
    }

    #[test]
    fn load_state_reconciles_missing_graph_toml() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        // Remove graph.toml to simulate pre-graph store
        let _ = fs::remove_file(store.graph_path());

        let auth = sample_component("auth");
        store
            .write_atomic(&lock, &store.component_path("auth"), &auth)
            .unwrap();

        let mut dec = sample_decision("token-format", "auth");
        dec.decision.tags = vec!["security".into(), "auth".into()];
        store
            .write_atomic(&lock, &store.decision_path("token-format"), &dec)
            .unwrap();

        drop(lock);
        let state = store.load_state().unwrap();
        // Should still build a graph index from files
        assert!(state.graph_index.nodes.iter().any(|n| n.name == "project"));
        assert!(state.graph_index.nodes.iter().any(|n| n.name == "auth"));
        // A decision's tags and owner live in its file, so no index is needed.
        let node = state
            .graph_index
            .nodes
            .iter()
            .find(|n| n.name == "token-format")
            .unwrap();
        assert_eq!(node.tags, ["security", "auth"]);
        assert!(
            state
                .graph_index
                .edges
                .iter()
                .any(|e| e.from == "token-format"
                    && e.to == "auth"
                    && e.kind == EdgeKind::BelongsTo)
        );
    }

    #[test]
    fn load_state_corrects_stale_belongs_to_edge() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let auth = sample_component("auth");
        store
            .write_atomic(&lock, &store.component_path("auth"), &auth)
            .unwrap();
        let db = sample_component("database");
        store
            .write_atomic(&lock, &store.component_path("database"), &db)
            .unwrap();

        // Decision belongs to "auth" per its file content.
        let dec = sample_decision("token-format", "auth");
        store
            .write_atomic(&lock, &store.decision_path("token-format"), &dec)
            .unwrap();

        // Write graph.toml with a STALE BelongsTo edge pointing to "database".
        let auth_hash = hash_file(&store.component_path("auth")).unwrap();
        let db_hash = hash_file(&store.component_path("database")).unwrap();
        let dec_hash = hash_file(&store.decision_path("token-format")).unwrap();
        let project_hash = hash_file(&store.root().join("project.toml")).unwrap();

        let index = GraphIndex {
            version: 1,
            rebuilt: None,
            nodes: vec![
                NodeEntry {
                    name: "project".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: project_hash,
                },
                NodeEntry {
                    name: "auth".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: auth_hash,
                },
                NodeEntry {
                    name: "database".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: db_hash,
                },
                NodeEntry {
                    name: "token-format".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: dec_hash,
                },
            ],
            edges: vec![EdgeEntry {
                from: "token-format".into(),
                to: "database".into(),
                kind: EdgeKind::BelongsTo,
            }],
        };
        fs::write(store.graph_path(), toml::to_string_pretty(&index).unwrap()).unwrap();

        drop(lock);
        let state = store.load_state().unwrap();
        // BelongsTo must now point to "auth" (from decision file), not "database".
        assert!(
            state
                .graph_index
                .edges
                .iter()
                .any(|e| e.from == "token-format"
                    && e.to == "auth"
                    && e.kind == EdgeKind::BelongsTo)
        );
        assert!(
            !state
                .graph_index
                .edges
                .iter()
                .any(|e| e.from == "token-format"
                    && e.to == "database"
                    && e.kind == EdgeKind::BelongsTo)
        );
    }

    #[test]
    fn load_state_skips_dangling_belongs_to() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        // Decision references a component that does not exist on disk.
        let dec = sample_decision("orphan-dec", "deleted-component");
        store
            .write_atomic(&lock, &store.decision_path("orphan-dec"), &dec)
            .unwrap();

        drop(lock);
        let state = store.load_state().unwrap();
        // No BelongsTo edge should reference the missing component.
        assert!(
            !state
                .graph_index
                .edges
                .iter()
                .any(|e| e.to == "deleted-component")
        );
    }

    #[test]
    fn load_state_sorts_edges() {
        use crate::store::schema::*;

        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let auth = sample_component("auth");
        store
            .write_atomic(&lock, &store.component_path("auth"), &auth)
            .unwrap();
        let db = sample_component("database");
        store
            .write_atomic(&lock, &store.component_path("database"), &db)
            .unwrap();

        // Write graph.toml with deliberately unsorted edges.
        let auth_hash = hash_file(&store.component_path("auth")).unwrap();
        let db_hash = hash_file(&store.component_path("database")).unwrap();
        let project_hash = hash_file(&store.root().join("project.toml")).unwrap();

        let index = GraphIndex {
            version: 1,
            rebuilt: None,
            nodes: vec![
                NodeEntry {
                    name: "database".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: db_hash,
                },
                NodeEntry {
                    name: "project".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: project_hash,
                },
                NodeEntry {
                    name: "auth".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: auth_hash,
                },
            ],
            edges: vec![
                EdgeEntry {
                    from: "database".into(),
                    to: "auth".into(),
                    kind: EdgeKind::ConnectsTo,
                },
                EdgeEntry {
                    from: "auth".into(),
                    to: "database".into(),
                    kind: EdgeKind::ConnectsTo,
                },
            ],
        };
        fs::write(store.graph_path(), toml::to_string_pretty(&index).unwrap()).unwrap();

        drop(lock);
        let state = store.load_state().unwrap();

        // Nodes sorted by name.
        let node_names: Vec<&str> = state
            .graph_index
            .nodes
            .iter()
            .map(|n| n.name.as_str())
            .collect();
        assert_eq!(node_names, vec!["auth", "database", "project"]);

        // Edges sorted by (from, to, kind).
        let edge_froms: Vec<&str> = state
            .graph_index
            .edges
            .iter()
            .map(|e| e.from.as_str())
            .collect();
        assert_eq!(edge_froms, vec!["auth", "database"]);
    }
}
