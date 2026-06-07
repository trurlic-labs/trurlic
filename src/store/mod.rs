#[allow(dead_code)] // query + cycle methods consumed incrementally through Phases 5-7
pub mod graph;
pub mod schema;

mod state;
mod write;

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::Utc;
use fs2::FileExt;
use serde::de::DeserializeOwned;

use crate::{Error, Result};

use schema::{EdgeEntry, EdgeKind, GraphIndex, NodeEntry, NodeKind};

// Re-exports — public API surface of the store module.
// Graph types (NodeKind, EdgeKind, etc.) are accessible via `store::schema::*`.
pub use schema::{
    COMPONENTS_DIR, ComponentFile, DECISIONS_DIR, DecisionFile, FORMAT_VERSION, GRAPH_FILE,
    PATTERNS_DIR, PatternFile, ProjectFile, STATE_DIR, STORE_DIR,
};
pub use state::{ProjectState, is_valid_kebab_case};

const LOCK_TIMEOUT: Duration = Duration::from_secs(5);

const LOCK_POLL_INTERVAL: Duration = Duration::from_millis(50);

// ── Hashing ─────────────────────────────────────────────────────────────────

/// BLAKE3 hash of raw file bytes, returned as lowercase hex.
pub(crate) fn hash_file(path: &Path) -> Result<String> {
    let bytes = fs::read(path)?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

/// BLAKE3 hash of an in-memory byte slice, returned as lowercase hex.
pub(crate) fn hash_bytes(data: &[u8]) -> String {
    blake3::hash(data).to_hex().to_string()
}

// ── Store ────────────────────────────────────────────────────────────────────

/// Handle to a `.trurl/` directory.
/// Read methods work without locking. Write methods require a [`StoreLock`]
/// passed as a proof parameter.
#[derive(Debug)]
pub struct Store {
    pub(super) root: PathBuf,
}

impl Store {
    pub fn discover(start: &Path) -> Result<Self> {
        let mut current = start.canonicalize()?;
        loop {
            let candidate = current.join(STORE_DIR);
            if candidate.is_dir() {
                return Ok(Self { root: candidate });
            }
            if !current.pop() {
                return Err(Error::StoreNotFound(start.to_path_buf()));
            }
        }
    }

    pub fn at(root: PathBuf) -> Self {
        Self { root }
    }

    // ── Path helpers ─────────────────────────────────────────────────────

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn components_dir(&self) -> PathBuf {
        self.root.join(COMPONENTS_DIR)
    }

    pub(crate) fn decisions_dir(&self) -> PathBuf {
        self.root.join(DECISIONS_DIR)
    }

    pub(crate) fn patterns_dir(&self) -> PathBuf {
        self.root.join(PATTERNS_DIR)
    }

    pub(crate) fn state_dir(&self) -> PathBuf {
        self.root.join(STATE_DIR)
    }

    pub(crate) fn tmp_dir(&self) -> PathBuf {
        self.state_dir().join("tmp")
    }

    pub(crate) fn lock_path(&self) -> PathBuf {
        self.state_dir().join("lock")
    }

    pub fn component_path(&self, name: &str) -> PathBuf {
        self.components_dir().join(format!("{name}.toml"))
    }

    pub fn decision_path(&self, name: &str) -> PathBuf {
        self.decisions_dir().join(format!("{name}.toml"))
    }

    pub fn pattern_path(&self, name: &str) -> PathBuf {
        self.patterns_dir().join(format!("{name}.toml"))
    }

    pub(crate) fn graph_path(&self) -> PathBuf {
        self.root.join(GRAPH_FILE)
    }

    // ── Path safety ─────────────────────────────────────────────────────

    /// Verify that `path` is inside the store root directory.
    /// Defense-in-depth: all store paths are derived from `self.root`, so
    /// this check should never fire in correct code. It guards against
    /// programming errors that would write or delete files outside `.trurl/`.
    fn verify_path(&self, path: &Path) -> Result<()> {
        // Reject parent-directory components before the prefix check.
        // `starts_with` on non-canonicalized paths does not prevent
        // traversal via `..` segments (e.g. `/root/../etc/shadow`).
        for component in path.components() {
            if component == std::path::Component::ParentDir {
                return Err(Error::Validation(format!(
                    "path contains parent-directory traversal: {}",
                    path.display()
                )));
            }
        }
        if path.starts_with(&self.root) {
            Ok(())
        } else {
            Err(Error::Validation(format!(
                "path escapes store root: {}",
                path.display()
            )))
        }
    }

    // ── Locking ──────────────────────────────────────────────────────────

    /// Acquire an exclusive advisory lock on `.trurl/`.
    /// Times out after 5 seconds. The lock is released when the returned
    /// [`StoreLock`] is dropped.
    pub fn lock(&self) -> Result<StoreLock> {
        use std::io::{Read, Seek, SeekFrom, Write};

        fs::create_dir_all(self.state_dir())?;

        let mut file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.lock_path())?;

        let deadline = Instant::now() + LOCK_TIMEOUT;

        loop {
            match file.try_lock_exclusive() {
                Ok(()) => {
                    let _ = file.set_len(0);
                    let _ = file.seek(SeekFrom::Start(0));
                    let _ = write!(file, "{}", std::process::id());
                    return Ok(StoreLock { _file: file });
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        let mut contents = String::new();
                        let _ = file.seek(SeekFrom::Start(0));
                        let _ = file.read_to_string(&mut contents);
                        let holder_pid = contents.trim().parse::<u32>().ok();

                        let detail = match holder_pid {
                            Some(pid) => format!("possibly held by PID {pid}"),
                            None => "another trurl process may be running".into(),
                        };
                        return Err(Error::LockTimeout {
                            timeout_secs: LOCK_TIMEOUT.as_secs(),
                            detail,
                        });
                    }
                    std::thread::sleep(LOCK_POLL_INTERVAL);
                }
                Err(e) => return Err(Error::Io(e)),
            }
        }
    }

    // ── Reading ──────────────────────────────────────────────────────────

    fn read_toml<T: DeserializeOwned>(&self, path: &Path) -> Result<T> {
        let content = fs::read_to_string(path)?;
        Ok(toml::from_str(&content)?)
    }

    pub fn read_project(&self) -> Result<ProjectFile> {
        self.read_toml(&self.root.join("project.toml"))
    }

    #[allow(dead_code)]
    pub fn read_component(&self, name: &str) -> Result<ComponentFile> {
        let path = self.component_path(name);
        match self.read_toml(&path) {
            Ok(file) => Ok(file),
            Err(Error::Io(e)) if e.kind() == ErrorKind::NotFound => Err(Error::Validation(
                format!("component `{name}` does not exist"),
            )),
            Err(e) => Err(e),
        }
    }

    #[allow(dead_code)]
    pub fn read_decision(&self, name: &str) -> Result<DecisionFile> {
        let path = self.decision_path(name);
        match self.read_toml(&path) {
            Ok(file) => Ok(file),
            Err(Error::Io(e)) if e.kind() == ErrorKind::NotFound => Err(Error::Validation(
                format!("decision `{name}` does not exist"),
            )),
            Err(e) => Err(e),
        }
    }

    #[allow(dead_code)]
    pub fn read_pattern(&self, name: &str) -> Result<PatternFile> {
        let path = self.pattern_path(name);
        match self.read_toml(&path) {
            Ok(file) => Ok(file),
            Err(Error::Io(e)) if e.kind() == ErrorKind::NotFound => Err(Error::Validation(
                format!("pattern `{name}` does not exist"),
            )),
            Err(e) => Err(e),
        }
    }

    pub fn list_components(&self) -> Result<Vec<String>> {
        state::list_toml_stems(&self.components_dir())
    }

    pub fn list_decisions(&self) -> Result<Vec<String>> {
        state::list_toml_stems(&self.decisions_dir())
    }

    pub fn list_patterns(&self) -> Result<Vec<String>> {
        state::list_toml_stems(&self.patterns_dir())
    }

    // ── load_state ──────────────────────────────────────────────────────

    pub fn load_state(&self) -> Result<ProjectState> {
        let project = self.read_project()?;

        let mut components = BTreeMap::new();
        for name in self.list_components()? {
            let file: ComponentFile = self.read_toml(&self.component_path(&name))?;
            components.insert(name, file);
        }

        let mut decisions = BTreeMap::new();
        for name in self.list_decisions()? {
            let file: DecisionFile = self.read_toml(&self.decision_path(&name))?;
            decisions.insert(name, file);
        }

        let mut patterns = BTreeMap::new();
        for name in self.list_patterns()? {
            let file: PatternFile = self.read_toml(&self.pattern_path(&name))?;
            patterns.insert(name, file);
        }

        let graph_index = self.load_graph_index(&components, &decisions, &patterns)?;

        Ok(ProjectState::new(
            project,
            components,
            decisions,
            patterns,
            graph_index,
        ))
    }

    /// Reconcile the on-disk graph index with actual node files.
    ///
    /// Reads `graph.toml` for edges that cannot be inferred from node files
    /// (ConnectsTo, Supersedes, DependsOn, etc.), rebuilds the node list from
    /// the actual files with fresh BLAKE3 hashes, preserves tags from existing
    /// nodes, filters dangling edges, and ensures BelongsTo edges for all
    /// decisions.
    fn load_graph_index(
        &self,
        components: &BTreeMap<String, ComponentFile>,
        decisions: &BTreeMap<String, DecisionFile>,
        patterns: &BTreeMap<String, PatternFile>,
    ) -> Result<schema::GraphIndex> {
        let graph_path = self.graph_path();

        // Load existing graph for edges that cannot be inferred from files.
        let existing: GraphIndex = if graph_path.exists() {
            self.read_toml(&graph_path)?
        } else {
            GraphIndex {
                version: 1,
                rebuilt: Utc::now(),
                nodes: vec![],
                edges: vec![],
            }
        };

        // Build lookup from existing index for O(1) tag preservation.
        let existing_tags: std::collections::HashMap<&str, &[String]> = existing
            .nodes
            .iter()
            .map(|n| (n.name.as_str(), n.tags.as_slice()))
            .collect();

        // Build node list from actual files, preserving tags from existing index.
        let mut nodes = Vec::new();

        // Project virtual node.
        let project_path = self.root.join("project.toml");
        let project_hash = hash_file(&project_path)?;
        let project_tags = existing_tags
            .get("project")
            .map(|t| t.to_vec())
            .unwrap_or_default();
        nodes.push(NodeEntry {
            name: "project".into(),
            kind: NodeKind::Component,
            tags: project_tags,
            hash: project_hash,
        });

        for name in components.keys() {
            let hash = hash_file(&self.component_path(name))?;
            let tags = existing_tags
                .get(name.as_str())
                .map(|t| t.to_vec())
                .unwrap_or_default();
            nodes.push(NodeEntry {
                name: name.clone(),
                kind: NodeKind::Component,
                tags,
                hash,
            });
        }

        for name in decisions.keys() {
            let hash = hash_file(&self.decision_path(name))?;
            let tags = existing_tags
                .get(name.as_str())
                .map(|t| t.to_vec())
                .unwrap_or_default();
            nodes.push(NodeEntry {
                name: name.clone(),
                kind: NodeKind::Decision,
                tags,
                hash,
            });
        }

        for name in patterns.keys() {
            let hash = hash_file(&self.pattern_path(name))?;
            let tags = existing_tags
                .get(name.as_str())
                .map(|t| t.to_vec())
                .unwrap_or_default();
            nodes.push(NodeEntry {
                name: name.clone(),
                kind: NodeKind::Pattern,
                tags,
                hash,
            });
        }

        // Preserve non-BelongsTo edges from existing graph that reference valid nodes.
        // BelongsTo edges are always re-derived from decision files (source of truth).
        // This prevents stale BelongsTo edges when decision.component is edited on disk.
        let valid_names: std::collections::HashSet<&str> =
            nodes.iter().map(|n| n.name.as_str()).collect();

        let mut edges: Vec<EdgeEntry> = existing
            .edges
            .into_iter()
            .filter(|e| {
                e.kind != EdgeKind::BelongsTo
                    && valid_names.contains(e.from.as_str())
                    && valid_names.contains(e.to.as_str())
            })
            .collect();

        // Re-derive BelongsTo edges from decision files.
        // Skip if the target component doesn't exist — validation will report it.
        for (name, dec) in decisions {
            let target = &dec.decision.component;
            if valid_names.contains(target.as_str()) {
                edges.push(EdgeEntry {
                    from: name.clone(),
                    to: target.clone(),
                    kind: EdgeKind::BelongsTo,
                });
            }
        }

        // Sort for deterministic output.
        nodes.sort_by(|a, b| a.name.cmp(&b.name));
        edges.sort_by(|a, b| (&a.from, &a.to, &a.kind).cmp(&(&b.from, &b.to, &b.kind)));

        Ok(GraphIndex {
            version: 1,
            rebuilt: Utc::now(),
            nodes,
            edges,
        })
    }

    // ── Version check ────────────────────────────────────────────────────

    pub fn check_version(&self) -> Result<()> {
        let project = self.read_project()?;
        let stored = &project.trurl_version;
        if stored == FORMAT_VERSION {
            return Ok(());
        }
        match compare_versions(stored, FORMAT_VERSION) {
            Ordering::Greater => Err(Error::Validation(format!(
                ".trurl/ format version `{stored}` is newer than this CLI \
                 (expected `{FORMAT_VERSION}`). Please upgrade trurl."
            ))),
            _ => Err(Error::Validation(format!(
                ".trurl/ format version `{stored}` is older than this CLI \
                 (expected `{FORMAT_VERSION}`). A format migration may be needed."
            ))),
        }
    }
}

// ── StoreLock ────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct StoreLock {
    _file: File,
}

// ── Version comparison ──────────────────────────────────────────────────────

fn compare_versions(a: &str, b: &str) -> Ordering {
    let parse = |v: &str| -> (u32, u32, u32) {
        let mut parts = v.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
        let major = parts.next().unwrap_or(0);
        let minor = parts.next().unwrap_or(0);
        let patch = parts.next().unwrap_or(0);
        (major, minor, patch)
    };
    parse(a).cmp(&parse(b))
}

// ── Test helpers ────────────────────────────────────────────────────────────

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use crate::store::schema::{
        Component, ComponentFile, Decision, DecisionFile, GraphIndex, NodeEntry, NodeKind, Project,
        ProjectFile,
    };
    use chrono::{TimeZone, Utc};

    pub fn setup_store(dir: &Path) -> Store {
        let root = dir.join(STORE_DIR);
        fs::create_dir_all(root.join(COMPONENTS_DIR)).unwrap();
        fs::create_dir_all(root.join(DECISIONS_DIR)).unwrap();
        fs::create_dir_all(root.join(PATTERNS_DIR)).unwrap();
        fs::create_dir_all(root.join(STATE_DIR)).unwrap();

        let project = ProjectFile {
            trurl_version: FORMAT_VERSION.into(),
            project: Project {
                name: "test-project".into(),
                description: "A test project".into(),
            },
        };
        let project_content = toml::to_string_pretty(&project).unwrap();
        fs::write(root.join("project.toml"), &project_content).unwrap();

        // Write initial graph.toml with the project virtual node.
        let project_hash = hash_bytes(project_content.as_bytes());
        let index = GraphIndex {
            version: 1,
            rebuilt: Utc::now(),
            nodes: vec![NodeEntry {
                name: "project".into(),
                kind: NodeKind::Component,
                tags: vec![],
                hash: project_hash,
            }],
            edges: vec![],
        };
        fs::write(
            root.join(GRAPH_FILE),
            toml::to_string_pretty(&index).unwrap(),
        )
        .unwrap();

        Store::at(root)
    }

    pub fn setup_store_with_version(dir: &Path, version: &str) -> Store {
        let root = dir.join(STORE_DIR);
        fs::create_dir_all(root.join(COMPONENTS_DIR)).unwrap();
        fs::create_dir_all(root.join(DECISIONS_DIR)).unwrap();
        fs::create_dir_all(root.join(PATTERNS_DIR)).unwrap();
        fs::create_dir_all(root.join(STATE_DIR)).unwrap();

        let project = ProjectFile {
            trurl_version: version.into(),
            project: Project {
                name: "test-project".into(),
                description: "A test project".into(),
            },
        };
        let project_content = toml::to_string_pretty(&project).unwrap();
        fs::write(root.join("project.toml"), &project_content).unwrap();

        let project_hash = hash_bytes(project_content.as_bytes());
        let index = GraphIndex {
            version: 1,
            rebuilt: Utc::now(),
            nodes: vec![NodeEntry {
                name: "project".into(),
                kind: NodeKind::Component,
                tags: vec![],
                hash: project_hash,
            }],
            edges: vec![],
        };
        fs::write(
            root.join(GRAPH_FILE),
            toml::to_string_pretty(&index).unwrap(),
        )
        .unwrap();

        Store::at(root)
    }

    pub fn sample_component(name: &str) -> ComponentFile {
        ComponentFile {
            component: Component {
                name: name.into(),
                description: format!("The {name} component"),
            },
        }
    }

    pub fn sample_decision(name: &str, component: &str) -> DecisionFile {
        DecisionFile {
            decision: Decision {
                component: component.into(),
                choice: format!("Choice for {name}"),
                reason: format!("Reason for {name}"),
                alternatives: vec![],
                created: Utc.with_ymd_and_hms(2025, 6, 1, 12, 0, 0).unwrap(),
            },
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;
    use std::cmp::Ordering;
    use tempfile::TempDir;

    // ── discover ─────────────────────────────────────────────────────────

    #[test]
    fn discover_finds_in_current_dir() {
        let tmp = TempDir::new().unwrap();
        setup_store(tmp.path());

        let store = Store::discover(tmp.path()).unwrap();
        assert_eq!(store.root(), tmp.path().join(STORE_DIR));
    }

    #[test]
    fn discover_finds_in_parent() {
        let tmp = TempDir::new().unwrap();
        setup_store(tmp.path());

        let nested = tmp.path().join("src").join("deep");
        fs::create_dir_all(&nested).unwrap();

        let store = Store::discover(&nested).unwrap();
        assert_eq!(store.root(), tmp.path().join(STORE_DIR));
    }

    #[test]
    fn discover_fails_when_absent() {
        let tmp = TempDir::new().unwrap();
        let err = Store::discover(tmp.path()).unwrap_err();
        assert!(matches!(err, Error::StoreNotFound(_)));
    }

    // ── lock ─────────────────────────────────────────────────────────────

    #[test]
    fn lock_acquire_and_release() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());

        {
            let _lock = store.lock().unwrap();
            assert!(store.lock_path().exists());
        }
    }

    #[test]
    fn lock_writes_pid_to_lock_file() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());

        let _lock = store.lock().unwrap();

        let content = fs::read_to_string(store.lock_path()).unwrap();
        let pid: u32 = content
            .trim()
            .parse()
            .expect("lock file should contain PID");
        assert_eq!(pid, std::process::id());
    }

    // ── read / write round-trip ──────────────────────────────────────────

    #[test]
    fn write_and_read_component() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let comp = sample_component("auth");
        let path = store.component_path("auth");
        store.write_atomic(&lock, &path, &comp).unwrap();

        let read_back = store.read_component("auth").unwrap();
        assert_eq!(comp, read_back);
    }

    #[test]
    fn write_and_read_decision() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let dec = sample_decision("error-strategy", "project");
        let path = store.decision_path("error-strategy");
        store.write_atomic(&lock, &path, &dec).unwrap();

        let read_back = store.read_decision("error-strategy").unwrap();
        assert_eq!(dec, read_back);
    }

    #[test]
    fn read_missing_component_gives_clear_error() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());

        let err = store.read_component("nonexistent").unwrap_err();
        match err {
            Error::Validation(msg) => assert!(msg.contains("nonexistent")),
            other => panic!("expected Validation error, got: {other}"),
        }
    }

    #[test]
    fn read_missing_decision_gives_clear_error() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());

        let err = store.read_decision("nonexistent").unwrap_err();
        match err {
            Error::Validation(msg) => assert!(msg.contains("nonexistent")),
            other => panic!("expected Validation, got: {other}"),
        }
    }

    // ── list / load_state ────────────────────────────────────────────────

    #[test]
    fn list_components_sorted() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        for name in &["database", "auth", "rate-limiter"] {
            let comp = sample_component(name);
            store
                .write_atomic(&lock, &store.component_path(name), &comp)
                .unwrap();
        }

        let names = store.list_components().unwrap();
        assert_eq!(names, vec!["auth", "database", "rate-limiter"]);
    }

    #[test]
    fn list_empty_dir_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());

        assert!(store.list_components().unwrap().is_empty());
        assert!(store.list_decisions().unwrap().is_empty());
        assert!(store.list_patterns().unwrap().is_empty());
    }

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

        let state = store.load_state().unwrap();
        // Should still build a graph index from files
        assert!(state.graph_index.nodes.iter().any(|n| n.name == "project"));
        assert!(state.graph_index.nodes.iter().any(|n| n.name == "auth"));
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
            rebuilt: Utc::now(),
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

    // ── check_version ────────────────────────────────────────────────────

    #[test]
    fn check_version_passes_on_match() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        store.check_version().unwrap();
    }

    #[test]
    fn check_version_rejects_newer_format() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store_with_version(tmp.path(), "99.0.0");

        let err = store.check_version().unwrap_err();
        match err {
            Error::Validation(msg) => {
                assert!(msg.contains("99.0.0"));
                assert!(msg.contains("newer"), "should mention 'newer': {msg}");
                assert!(msg.contains("upgrade"), "should suggest upgrade: {msg}");
            }
            other => panic!("expected Validation, got: {other}"),
        }
    }

    #[test]
    fn check_version_rejects_older_format() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store_with_version(tmp.path(), "0.0.1");

        let err = store.check_version().unwrap_err();
        match err {
            Error::Validation(msg) => {
                assert!(msg.contains("0.0.1"));
                assert!(msg.contains("older"), "should mention 'older': {msg}");
                assert!(msg.contains("migration"), "should mention migration: {msg}");
            }
            other => panic!("expected Validation, got: {other}"),
        }
    }

    // ── verify_path ──────────────────────────────────────────────────────

    #[test]
    fn verify_path_accepts_child() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        store.verify_path(&store.component_path("auth")).unwrap();
    }

    #[test]
    fn verify_path_rejects_outside() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let outside = tmp.path().join("outside.toml");
        let err = store.verify_path(&outside).unwrap_err();
        assert!(matches!(err, Error::Validation(_)));
    }

    #[test]
    fn verify_path_rejects_traversal() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let traversal = store.root().join("..").join("escape.toml");
        let err = store.verify_path(&traversal).unwrap_err();
        match err {
            Error::Validation(msg) => {
                assert!(
                    msg.contains("parent-directory"),
                    "should mention traversal: {msg}"
                );
            }
            other => panic!("expected Validation, got: {other}"),
        }
    }

    // ── compare_versions ─────────────────────────────────────────────────

    #[test]
    fn compare_versions_equal() {
        assert_eq!(compare_versions("0.2.0", "0.2.0"), Ordering::Equal);
        assert_eq!(compare_versions("1.2.3", "1.2.3"), Ordering::Equal);
    }

    #[test]
    fn compare_versions_major_dominates() {
        assert_eq!(compare_versions("2.0.0", "1.9.9"), Ordering::Greater);
        assert_eq!(compare_versions("0.9.9", "1.0.0"), Ordering::Less);
    }

    #[test]
    fn compare_versions_minor() {
        assert_eq!(compare_versions("0.2.0", "0.1.9"), Ordering::Greater);
        assert_eq!(compare_versions("0.0.9", "0.1.0"), Ordering::Less);
    }

    #[test]
    fn compare_versions_patch() {
        assert_eq!(compare_versions("0.1.1", "0.1.0"), Ordering::Greater);
        assert_eq!(compare_versions("0.1.0", "0.1.1"), Ordering::Less);
    }

    #[test]
    fn compare_versions_malformed_defaults_to_zero() {
        assert_eq!(compare_versions("abc", "0.0.0"), Ordering::Equal);
        assert_eq!(compare_versions("1", "1.0.0"), Ordering::Equal);
        assert_eq!(compare_versions("1.2", "1.2.0"), Ordering::Equal);
    }

    // ── hash ─────────────────────────────────────────────────────────────

    #[test]
    fn hash_bytes_is_deterministic() {
        let a = hash_bytes(b"hello world");
        let b = hash_bytes(b"hello world");
        assert_eq!(a, b);
        assert_eq!(a.len(), 64); // 256-bit hex
    }

    #[test]
    fn hash_file_matches_hash_bytes() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("test.txt");
        let content = b"test content";
        fs::write(&path, content).unwrap();

        let file_hash = hash_file(&path).unwrap();
        let bytes_hash = hash_bytes(content);
        assert_eq!(file_hash, bytes_hash);
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
            rebuilt: Utc::now(),
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
