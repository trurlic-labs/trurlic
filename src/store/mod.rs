mod access;
pub(crate) mod cascade;
mod commit;
#[cfg(any(test, feature = "bench"))]
pub mod corpus;
mod create;
mod cycles;
mod durable;
pub(crate) mod failpoint;
pub mod graph;
mod journal;
pub(crate) mod limits;
mod load;
mod lock;
mod migration;
mod pattern_removal;
mod query;
mod recovery;
pub mod schema;
mod validate;

mod state;
pub(crate) mod text;
mod time;
mod version;
mod write;

use std::fs;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;

use crate::{Error, Result};

use schema::{GraphIndex, NodeKind};

// Re-exports — public API surface of the store module.
// Graph types (NodeKind, EdgeKind, etc.) are accessible via `store::schema::*`.
pub use access::Writer;
pub use lock::StoreLock;
pub(crate) use migration::{Loss, MigrationReport};
pub use schema::{
    COMPONENTS_DIR, CodeRef, ComponentFile, DECISIONS_DIR, DecisionFile, FORMAT_VERSION,
    GRAPH_FILE, PATTERNS_DIR, PatternFile, ProjectFile, STATE_DIR, STORE_DIR,
};
pub use state::{ProjectState, has_control_chars, is_valid_kebab_case, slugify};
pub use time::{Timestamp, TimestampError};
pub use version::FormatVersion;
pub use write::{RecordDecisionParams, RecordPatternParams, ReviseDecisionParams};

/// Syntactic validation for a code reference.
///
/// Paths are NOT validated against the filesystem — the decision may
/// reference files that don't exist yet (design-first) or files that
/// were deleted (drift).
pub(crate) fn validate_code_ref(cr: &CodeRef) -> Result<()> {
    use limits::{MAX_CODE_REF_PATH_BYTES, MAX_CODE_REF_SYMBOL_BYTES};

    if cr.file.trim().is_empty() {
        return Err(Error::Validation(
            "code_ref file path cannot be empty".into(),
        ));
    }
    if cr.file.len() > MAX_CODE_REF_PATH_BYTES {
        return Err(Error::Validation(format!(
            "code_ref file path exceeds {MAX_CODE_REF_PATH_BYTES} byte limit"
        )));
    }
    if cr.file.starts_with('/')
        || cr
            .file
            .split('/')
            .next()
            .is_some_and(|seg| seg.ends_with(':'))
        || std::path::Path::new(&cr.file).is_absolute()
        || matches!(
            std::path::Path::new(&cr.file).components().next(),
            Some(std::path::Component::Prefix(_))
        )
    {
        return Err(Error::Validation(
            "code_ref file path must be relative to the project root".into(),
        ));
    }
    // Component-wise check: reject a `..` path segment (traversal) without
    // rejecting legitimate filenames that merely contain `..` (e.g. `a..b.rs`).
    if cr.file.split('/').any(|segment| segment == "..") {
        return Err(Error::Validation(
            "code_ref file path must not contain path traversal (..)".into(),
        ));
    }
    if cr.file.contains('\\') {
        return Err(Error::Validation(
            "code_ref file path must use forward slashes".into(),
        ));
    }
    if has_control_chars(&cr.file) {
        return Err(Error::Validation(
            "code_ref file path contains control characters".into(),
        ));
    }
    if let Some(sym) = &cr.symbol {
        if sym.trim().is_empty() {
            return Err(Error::Validation(
                "code_ref symbol must not be empty".into(),
            ));
        }
        if sym.len() > MAX_CODE_REF_SYMBOL_BYTES {
            return Err(Error::Validation(format!(
                "code_ref symbol exceeds {MAX_CODE_REF_SYMBOL_BYTES} byte limit"
            )));
        }
        if has_control_chars(sym) {
            return Err(Error::Validation(
                "code_ref symbol contains control characters".into(),
            ));
        }
    }
    Ok(())
}

/// Validate a full set of code references: count limit plus per-ref syntax.
///
/// The single slice-level validator. Every write path (the store methods
/// and MCP `parse_code_refs`) calls this so the `MAX_CODE_REFS` cap and
/// per-ref rules are enforced identically. The store invokes it as the final
/// trust boundary no write can bypass.
pub(crate) fn validate_code_refs(refs: &[CodeRef]) -> Result<()> {
    if refs.len() > limits::MAX_CODE_REFS {
        return Err(Error::Validation(format!(
            "code_refs has too many items ({}, max {})",
            refs.len(),
            limits::MAX_CODE_REFS
        )));
    }
    for cr in refs {
        validate_code_ref(cr)?;
    }
    Ok(())
}

/// Serialize code references to JSON `Value` array.
///
/// Single serialization path — every module that needs `CodeRef` as
/// JSON calls this instead of hand-building `match &cr.symbol` arms.
pub(crate) fn code_refs_to_json(refs: &[CodeRef]) -> Vec<serde_json::Value> {
    refs.iter()
        .map(|cr| {
            let mut obj = serde_json::json!({ "file": cr.file });
            if let Some(sym) = &cr.symbol {
                obj["symbol"] = serde_json::json!(sym);
            }
            obj
        })
        .collect()
}

/// Normalize a file path query for `decisions_for_file`.
///
/// Strips leading `./`, collapses multiple `/` to single `/`, and strips
/// trailing `/`. Validates the result against the same syntactic rules as
/// [`validate_code_ref`] (reuses it directly) — an invalid query is an error,
/// not an empty result. Returns the normalized path on success.
pub(crate) fn normalize_file_query(path: &str) -> Result<String> {
    let trimmed = path.strip_prefix("./").unwrap_or(path);

    // Collapse runs of `/` to a single `/`.
    let mut normalized = String::with_capacity(trimmed.len());
    let mut prev_slash = false;
    for ch in trimmed.chars() {
        if ch == '/' {
            if !prev_slash {
                normalized.push('/');
            }
            prev_slash = true;
        } else {
            normalized.push(ch);
            prev_slash = false;
        }
    }

    // Strip trailing slash (directory queries don't need it for prefix matching).
    if normalized.ends_with('/') {
        normalized.pop();
    }

    let cr = CodeRef {
        file: normalized.clone(),
        symbol: None,
    };
    validate_code_ref(&cr)?;
    Ok(normalized)
}

/// Format code references as a comma-separated display string.
///
/// Returns `"file::symbol, file2"` — callers add their own prefix
/// and newline.
pub(crate) fn format_code_refs(refs: &[CodeRef]) -> String {
    refs.iter()
        .map(|cr| match &cr.symbol {
            Some(sym) => format!("{}::{}", cr.file, sym),
            None => cr.file.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Whether every code reference a decision carries points at a file confirmed
/// absent from disk — the decision's anchors have all been deleted.
///
/// A decision with no code refs is never orphaned: the absence of a link is not
/// a broken link. Probing uses `try_exists`, and only a definite `Ok(false)`
/// counts as missing — an `Err` (permission denied, mount not ready, transient
/// I/O) is treated as *present*, so a filesystem blip can never misreport a live
/// file as deleted and drive a decision to be flagged as having orphaned refs or
/// garbage-collected.
///
/// This is the single source of truth for reference-orphaning, shared by the
/// context health report and the `gc` collector so both agree on the predicate.
pub(crate) fn decision_refs_all_missing(project_root: &Path, dec: &DecisionFile) -> bool {
    let refs = &dec.decision.code_refs;
    !refs.is_empty()
        && refs
            .iter()
            .all(|code_ref| matches!(project_root.join(&code_ref.file).try_exists(), Ok(false)))
}

// ── Hashing ─────────────────────────────────────────────────────────────────

/// BLAKE3 hash of raw file bytes, returned as lowercase hex.
pub(crate) fn hash_file(path: &Path) -> Result<String> {
    let bytes = fs::read(path).map_err(Error::io(path))?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

/// BLAKE3 hash of an in-memory byte slice, returned as lowercase hex.
#[must_use]
pub fn hash_bytes(data: &[u8]) -> String {
    blake3::hash(data).to_hex().to_string()
}

/// The first 12 hex digits of `hash`, as messages show it.
fn abbreviate(hash: &str) -> &str {
    hash.get(..12).unwrap_or(hash)
}

// ── Store ────────────────────────────────────────────────────────────────────

/// Handle to a `.trurlic/` directory. It holds no graph: a read loads one
/// with [`Store::load_state`], and every graph write goes through the
/// [`Writer`] that [`Store::begin_write`] returns.
#[derive(Debug)]
pub struct Store {
    pub(super) root: PathBuf,
}

impl Store {
    pub fn discover(start: &Path) -> Result<Self> {
        let mut current = start.canonicalize().map_err(Error::io(start))?;
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
    /// programming errors that would write or delete files outside `.trurlic/`.
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

    // ── Reading ──────────────────────────────────────────────────────────

    fn read_toml<T: DeserializeOwned>(&self, path: &Path) -> Result<T> {
        let bytes = fs::read(path).map_err(Error::io(path))?;
        toml::from_slice(&bytes).map_err(Error::toml(path))
    }

    #[cfg(test)]
    pub fn read_component(&self, name: &str) -> Result<ComponentFile> {
        let path = self.component_path(name);
        match self.read_toml(&path) {
            Ok(file) => Ok(file),
            Err(Error::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
                Err(Error::ComponentNotFound(name.into()))
            }
            Err(e) => Err(e),
        }
    }

    #[cfg(test)]
    pub fn read_decision(&self, name: &str) -> Result<DecisionFile> {
        let path = self.decision_path(name);
        match self.read_toml(&path) {
            Ok(file) => Ok(file),
            Err(Error::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
                Err(Error::DecisionNotFound(name.into()))
            }
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

    // ── Hash verification ────────────────────────────────────────────────

    /// What `trurlic check` reports, from one read: each node whose file no
    /// longer matches the hash `graph.toml` stores for it, then every issue
    /// of the loaded graph.
    ///
    /// The hashes are compared against `graph.toml` as it is on disk, not
    /// against the index a load reconciles in memory, so this is where a
    /// node file edited outside Trurlic, or bitrot, shows up.
    pub fn verify(&self) -> Result<Vec<graph::Issue>> {
        self.read(|project, project_hash| {
            let mut issues = self.hash_issues()?;
            issues.extend(self.read_graph(project, project_hash)?.validate());
            Ok(issues)
        })
    }

    #[cfg(test)]
    pub(crate) fn verify_hashes(&self) -> Result<Vec<graph::Issue>> {
        self.read(|_, _| self.hash_issues())
    }

    fn hash_issues(&self) -> Result<Vec<graph::Issue>> {
        let graph_path = self.graph_path();
        if !graph_path.exists() {
            return Ok(vec![graph::Issue {
                kind: graph::IssueKind::IndexMissing,
                subject: GRAPH_FILE.into(),
                message: "graph.toml is missing (will be rebuilt from node files)".into(),
            }]);
        }

        let index: GraphIndex = self.read_toml(&graph_path)?;
        let mut issues = Vec::new();

        for node in index.nodes {
            let path = match node.kind {
                NodeKind::Component if node.name == "project" => self.root.join("project.toml"),
                NodeKind::Component => self.component_path(&node.name),
                NodeKind::Decision => self.decision_path(&node.name),
                NodeKind::Pattern => self.pattern_path(&node.name),
            };

            let (kind, message) = match hash_file(&path) {
                Ok(actual) if actual != node.hash => (
                    graph::IssueKind::HashMismatch,
                    format!(
                        "{} `{}` content changed since last indexed \
                         (stored: {}..., actual: {}...)",
                        node.kind.as_str(),
                        node.name,
                        abbreviate(&node.hash),
                        abbreviate(&actual),
                    ),
                ),
                Ok(_) => continue, // hash matches
                Err(_) => (
                    graph::IssueKind::NodeFileMissing,
                    format!(
                        "{} `{}` is in graph.toml but the file is missing or unreadable",
                        node.kind.as_str(),
                        node.name,
                    ),
                ),
            };
            issues.push(graph::Issue {
                kind,
                subject: node.name,
                message,
            });
        }

        Ok(issues)
    }
}

// ── Test helpers ────────────────────────────────────────────────────────────

#[cfg(test)]
pub(crate) mod testing {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use super::*;
    use crate::store::schema::{
        Attribution, Component, ComponentFile, Decision, DecisionFile, EdgeEntry, EdgeKind,
        GraphIndex, NodeEntry, NodeKind, Project, ProjectFile,
    };

    pub fn setup_store(dir: &Path) -> Store {
        setup_store_with_version(dir, &FORMAT_VERSION.to_string())
    }

    /// An empty store whose `project.toml` states `version` as its
    /// `format_version`, whatever the text.
    pub fn setup_store_with_version(dir: &Path, version: &str) -> Store {
        let root = dir.join(STORE_DIR);
        fs::create_dir_all(root.join(COMPONENTS_DIR)).unwrap();
        fs::create_dir_all(root.join(DECISIONS_DIR)).unwrap();
        fs::create_dir_all(root.join(PATTERNS_DIR)).unwrap();
        fs::create_dir_all(root.join(STATE_DIR)).unwrap();

        let mut project = toml::Table::new();
        project.insert("format_version".into(), version.into());
        let mut fields = toml::Table::new();
        fields.insert("name".into(), "test-project".into());
        fields.insert("description".into(), "A test project".into());
        project.insert("project".into(), fields.into());
        let project_content = toml::to_string_pretty(&project).unwrap();
        fs::write(root.join("project.toml"), &project_content).unwrap();

        let project_hash = hash_bytes(project_content.as_bytes());
        let index = GraphIndex {
            version: 1,
            rebuilt: None,
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
                tags: vec![],
                attribution: Attribution::User,
                created: "2025-06-01T12:00:00Z".parse::<Timestamp>().unwrap(),
                code_refs: vec![],
                history: vec![],
            },
        }
    }

    /// Minimal [`ProjectState`] with no components, decisions, or patterns.
    /// The shared starting point for tests that build state incrementally.
    pub fn empty_project_state() -> super::state::ProjectState {
        use crate::store::schema::FORMAT_VERSION;
        super::state::ProjectState::new(
            ProjectFile {
                format_version: FORMAT_VERSION,
                project: Project {
                    name: "test".into(),
                    description: String::new(),
                },
            },
            BTreeMap::new(),
            BTreeMap::new(),
            BTreeMap::new(),
            GraphIndex {
                version: 1,
                rebuilt: None,
                nodes: vec![],
                edges: vec![],
            },
        )
    }

    /// Full test state: auth + database + rate-limiter, 3 decisions
    /// (use-jwt, error-strategy, db-pool), graph tags, ConnectsTo edges.
    ///
    /// This is the "kitchen sink" fixture for tests that need a realistic
    /// graph without constructing one from scratch every time.
    pub fn rich_test_state() -> super::state::ProjectState {
        use crate::store::schema::{EdgeEntry, EdgeKind, FORMAT_VERSION, NodeKind};

        let ts = "2025-06-01T10:00:00Z".parse::<Timestamp>().unwrap();

        let mut components = BTreeMap::new();
        for (name, desc) in [
            ("auth", "Authentication and token management"),
            ("database", "Database access layer"),
            ("rate-limiter", "Request rate limiting"),
        ] {
            components.insert(
                name.into(),
                Arc::new(ComponentFile {
                    component: Component {
                        name: name.into(),
                        description: desc.into(),
                    },
                }),
            );
        }

        let mut decisions = BTreeMap::new();
        decisions.insert(
            "use-jwt".into(),
            Arc::new(DecisionFile {
                decision: Decision {
                    component: "auth".into(),
                    choice: "JWT with DPoP binding".into(),
                    reason: "Stateless, no session store needed".into(),
                    alternatives: vec!["Session cookies — rejected: server state".into()],
                    tags: vec![],
                    attribution: Attribution::User,
                    created: ts,
                    code_refs: vec![],
                    history: vec![],
                },
            }),
        );
        decisions.insert(
            "error-strategy".into(),
            Arc::new(DecisionFile {
                decision: Decision {
                    component: "project".into(),
                    choice: "ALL error handling MUST use Result<T, AppError>".into(),
                    reason: "Consistent error propagation".into(),
                    alternatives: vec![],
                    tags: vec![],
                    attribution: Attribution::User,
                    created: ts,
                    code_refs: vec![],
                    history: vec![],
                },
            }),
        );
        decisions.insert(
            "db-pool".into(),
            Arc::new(DecisionFile {
                decision: Decision {
                    component: "database".into(),
                    choice: "Shared connection pool via app state".into(),
                    reason: "Avoid per-request connection overhead".into(),
                    alternatives: vec![],
                    tags: vec![],
                    attribution: Attribution::User,
                    created: ts,
                    code_refs: vec![],
                    history: vec![],
                },
            }),
        );

        let graph_index = GraphIndex {
            version: 1,
            rebuilt: None,
            nodes: vec![
                NodeEntry {
                    name: "project".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: String::new(),
                },
                NodeEntry {
                    name: "auth".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: String::new(),
                },
                NodeEntry {
                    name: "database".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: String::new(),
                },
                NodeEntry {
                    name: "rate-limiter".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: String::new(),
                },
                NodeEntry {
                    name: "use-jwt".into(),
                    kind: NodeKind::Decision,
                    tags: vec!["auth".into(), "security".into()],
                    hash: String::new(),
                },
                NodeEntry {
                    name: "error-strategy".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: String::new(),
                },
                NodeEntry {
                    name: "db-pool".into(),
                    kind: NodeKind::Decision,
                    tags: vec!["storage".into()],
                    hash: String::new(),
                },
            ],
            edges: vec![
                EdgeEntry {
                    from: "auth".into(),
                    to: "database".into(),
                    kind: EdgeKind::ConnectsTo,
                },
                EdgeEntry {
                    from: "rate-limiter".into(),
                    to: "database".into(),
                    kind: EdgeKind::ConnectsTo,
                },
                EdgeEntry {
                    from: "use-jwt".into(),
                    to: "auth".into(),
                    kind: EdgeKind::BelongsTo,
                },
                EdgeEntry {
                    from: "error-strategy".into(),
                    to: "project".into(),
                    kind: EdgeKind::BelongsTo,
                },
                EdgeEntry {
                    from: "db-pool".into(),
                    to: "database".into(),
                    kind: EdgeKind::BelongsTo,
                },
            ],
        };

        super::state::ProjectState::new(
            ProjectFile {
                format_version: FORMAT_VERSION,
                project: Project {
                    name: "test-project".into(),
                    description: "A test project".into(),
                },
            },
            components,
            decisions,
            BTreeMap::new(),
            graph_index,
        )
    }

    /// Create a store on disk with named components and return it loaded.
    ///
    /// For integration tests that need a real on-disk store without going
    /// through the `commands` module. Components are written atomically
    /// and state is loaded with a reconciled graph index.
    pub fn setup_store_with_components(
        dir: &Path,
        components: &[(&str, &str)],
    ) -> (Store, super::state::ProjectState) {
        let store = setup_store(dir);
        let lock = store.lock().unwrap();
        for &(name, description) in components {
            let comp = ComponentFile {
                component: Component {
                    name: name.into(),
                    description: description.into(),
                },
            };
            store
                .write_atomic(&lock, &store.component_path(name), &comp)
                .unwrap();
        }
        drop(lock);
        let state = store.load_state().unwrap();
        (store, state)
    }

    // ── Shared graph fixtures ────────────────────────────────────────────

    pub fn ts() -> Timestamp {
        "2025-06-01T10:00:00Z".parse().unwrap()
    }

    pub fn arc_map<T>(map: BTreeMap<String, T>) -> BTreeMap<String, Arc<T>> {
        map.into_iter().map(|(k, v)| (k, Arc::new(v))).collect()
    }

    /// Project-wide decisions joined by `depends_on` edges in the given
    /// order: a graph whose only issues are the ones the edges make.
    pub fn depends_on_graph(edges: &[(&str, &str)]) -> graph::InMemoryGraph {
        let mut names: Vec<&str> = edges.iter().flat_map(|&(from, to)| [from, to]).collect();
        names.sort_unstable();
        names.dedup();
        let mut index = GraphIndex {
            version: 1,
            rebuilt: None,
            nodes: vec![NodeEntry {
                name: "project".into(),
                kind: NodeKind::Component,
                tags: vec![],
                hash: String::new(),
            }],
            edges: Vec::new(),
        };
        let mut decisions = BTreeMap::new();
        for &name in &names {
            index.nodes.push(NodeEntry {
                name: name.into(),
                kind: NodeKind::Decision,
                tags: vec![],
                hash: String::new(),
            });
            index.edges.push(EdgeEntry {
                from: name.into(),
                to: "project".into(),
                kind: EdgeKind::BelongsTo,
            });
            decisions.insert(name.to_owned(), sample_decision(name, "project"));
        }
        index
            .edges
            .extend(edges.iter().map(|&(from, to)| EdgeEntry {
                from: from.into(),
                to: to.into(),
                kind: EdgeKind::DependsOn,
            }));
        graph::InMemoryGraph::build(
            &index,
            &BTreeMap::new(),
            &arc_map(decisions),
            &BTreeMap::new(),
        )
    }

    /// Realistic graph fixture shared by graph, query, and validate tests.
    ///
    ///   Components: project (virtual), auth, database, rate-limiter
    ///   Decisions: use-jwt (auth), error-strategy (project), db-pool (database)
    ///   Edges: auth→database (ConnectsTo), rate-limiter→database (ConnectsTo),
    ///          BelongsTo for all decisions
    pub fn test_graph() -> graph::InMemoryGraph {
        let index = GraphIndex {
            version: 1,
            rebuilt: None,
            nodes: vec![
                NodeEntry {
                    name: "project".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: "p".into(),
                },
                NodeEntry {
                    name: "auth".into(),
                    kind: NodeKind::Component,
                    tags: vec!["security".into()],
                    hash: "a".into(),
                },
                NodeEntry {
                    name: "database".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: "d".into(),
                },
                NodeEntry {
                    name: "rate-limiter".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: "r".into(),
                },
                NodeEntry {
                    name: "use-jwt".into(),
                    kind: NodeKind::Decision,
                    tags: vec!["auth".into()],
                    hash: "j".into(),
                },
                NodeEntry {
                    name: "error-strategy".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: "e".into(),
                },
                NodeEntry {
                    name: "db-pool".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: "b".into(),
                },
            ],
            edges: vec![
                EdgeEntry {
                    from: "auth".into(),
                    to: "database".into(),
                    kind: EdgeKind::ConnectsTo,
                },
                EdgeEntry {
                    from: "rate-limiter".into(),
                    to: "database".into(),
                    kind: EdgeKind::ConnectsTo,
                },
                EdgeEntry {
                    from: "use-jwt".into(),
                    to: "auth".into(),
                    kind: EdgeKind::BelongsTo,
                },
                EdgeEntry {
                    from: "error-strategy".into(),
                    to: "project".into(),
                    kind: EdgeKind::BelongsTo,
                },
                EdgeEntry {
                    from: "db-pool".into(),
                    to: "database".into(),
                    kind: EdgeKind::BelongsTo,
                },
            ],
        };

        let mut components = BTreeMap::new();
        for name in ["auth", "database", "rate-limiter"] {
            components.insert(
                name.into(),
                ComponentFile {
                    component: Component {
                        name: name.into(),
                        description: format!("The {name} component"),
                    },
                },
            );
        }

        let mut decisions = BTreeMap::new();
        decisions.insert(
            "use-jwt".into(),
            DecisionFile {
                decision: Decision {
                    component: "auth".into(),
                    choice: "JWT with DPoP".into(),
                    reason: "Stateless".into(),
                    alternatives: vec![],
                    tags: vec![],
                    attribution: Attribution::User,
                    created: ts(),
                    code_refs: vec![],
                    history: vec![],
                },
            },
        );
        decisions.insert(
            "error-strategy".into(),
            DecisionFile {
                decision: Decision {
                    component: "project".into(),
                    choice: "Result<T, AppError>".into(),
                    reason: "Consistent errors".into(),
                    alternatives: vec![],
                    tags: vec![],
                    attribution: Attribution::User,
                    created: ts(),
                    code_refs: vec![],
                    history: vec![],
                },
            },
        );
        decisions.insert(
            "db-pool".into(),
            DecisionFile {
                decision: Decision {
                    component: "database".into(),
                    choice: "Shared connection pool".into(),
                    reason: "Avoid per-request overhead".into(),
                    alternatives: vec![],
                    tags: vec![],
                    attribution: Attribution::User,
                    created: ts(),
                    code_refs: vec![],
                    history: vec![],
                },
            },
        );

        graph::InMemoryGraph::build(
            &index,
            &arc_map(components),
            &arc_map(decisions),
            &BTreeMap::new(),
        )
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;
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
        assert!(matches!(err, Error::ComponentNotFound(ref n) if n == "nonexistent"));
    }

    #[test]
    fn read_missing_decision_gives_clear_error() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());

        let err = store.read_decision("nonexistent").unwrap_err();
        assert!(matches!(err, Error::DecisionNotFound(ref n) if n == "nonexistent"));
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

    // ── hash ─────────────────────────────────────────────────────────────

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

    // ── validate_code_ref ──────────────────────────────────────────────

    #[test]
    fn validate_code_ref_valid_with_symbol() {
        let cr = CodeRef {
            file: "src/store/write.rs".into(),
            symbol: Some("content_hash".into()),
        };
        assert!(validate_code_ref(&cr).is_ok());
    }

    #[test]
    fn validate_code_ref_valid_without_symbol() {
        let cr = CodeRef {
            file: "src/store/write.rs".into(),
            symbol: None,
        };
        assert!(validate_code_ref(&cr).is_ok());
    }

    #[test]
    fn validate_code_ref_rejects_empty_path() {
        let cr = CodeRef {
            file: String::new(),
            symbol: None,
        };
        let err = validate_code_ref(&cr).unwrap_err().to_string();
        assert!(err.contains("empty"), "{err}");
    }

    #[test]
    fn validate_code_ref_rejects_absolute_path() {
        let cr = CodeRef {
            file: "/etc/passwd".into(),
            symbol: None,
        };
        let err = validate_code_ref(&cr).unwrap_err().to_string();
        assert!(err.contains("relative to the project root"), "{err}");
    }

    #[test]
    fn validate_code_ref_rejects_path_traversal() {
        let cr = CodeRef {
            file: "../escape/file.rs".into(),
            symbol: None,
        };
        let err = validate_code_ref(&cr).unwrap_err().to_string();
        assert!(err.contains("traversal"), "{err}");
    }

    #[test]
    fn validate_code_ref_rejects_backslashes() {
        let cr = CodeRef {
            file: "src\\store\\write.rs".into(),
            symbol: None,
        };
        let err = validate_code_ref(&cr).unwrap_err().to_string();
        assert!(err.contains("forward slashes"), "{err}");
    }

    #[test]
    fn validate_code_ref_rejects_control_chars_in_path() {
        let cr = CodeRef {
            file: "src/store\x00write.rs".into(),
            symbol: None,
        };
        let err = validate_code_ref(&cr).unwrap_err().to_string();
        assert!(err.contains("control"), "{err}");
    }

    #[test]
    fn validate_code_ref_rejects_control_chars_in_symbol() {
        let cr = CodeRef {
            file: "src/store/write.rs".into(),
            symbol: Some("fn\x00bad".into()),
        };
        let err = validate_code_ref(&cr).unwrap_err().to_string();
        assert!(err.contains("control"), "{err}");
    }

    #[test]
    fn validate_code_ref_rejects_long_path() {
        let cr = CodeRef {
            file: "x".repeat(501),
            symbol: None,
        };
        let err = validate_code_ref(&cr).unwrap_err().to_string();
        assert!(err.contains("500"), "{err}");
    }

    #[test]
    fn validate_code_ref_rejects_long_symbol() {
        let cr = CodeRef {
            file: "src/lib.rs".into(),
            symbol: Some("x".repeat(201)),
        };
        let err = validate_code_ref(&cr).unwrap_err().to_string();
        assert!(err.contains("200"), "{err}");
    }

    #[test]
    fn validate_code_ref_rejects_empty_symbol() {
        let cr = CodeRef {
            file: "src/lib.rs".into(),
            symbol: Some(String::new()),
        };
        let err = validate_code_ref(&cr).unwrap_err().to_string();
        assert!(err.contains("empty"), "{err}");
    }

    #[test]
    fn validate_code_ref_rejects_whitespace_only_path() {
        let cr = CodeRef {
            file: "   ".into(),
            symbol: None,
        };
        let err = validate_code_ref(&cr).unwrap_err().to_string();
        assert!(err.contains("empty"), "{err}");
    }

    #[test]
    fn validate_code_ref_accepts_dots_within_filename() {
        // `..` inside a filename is not traversal — only a `..` path segment is.
        let cr = CodeRef {
            file: "src/store/a..b.rs".into(),
            symbol: None,
        };
        assert!(validate_code_ref(&cr).is_ok());
    }

    #[test]
    fn validate_code_ref_rejects_windows_drive_letter_uppercase() {
        let cr = CodeRef {
            file: "C:/Users/x/file.rs".into(),
            symbol: None,
        };
        let err = validate_code_ref(&cr).unwrap_err().to_string();
        assert!(err.contains("relative to the project root"), "{err}");
    }

    #[test]
    fn validate_code_ref_rejects_windows_drive_letter_lowercase() {
        let cr = CodeRef {
            file: "c:/x.rs".into(),
            symbol: None,
        };
        let err = validate_code_ref(&cr).unwrap_err().to_string();
        assert!(err.contains("relative to the project root"), "{err}");
    }

    #[test]
    fn validate_code_ref_rejects_bare_drive_letter() {
        let cr = CodeRef {
            file: "D:".into(),
            symbol: None,
        };
        let err = validate_code_ref(&cr).unwrap_err().to_string();
        assert!(err.contains("relative to the project root"), "{err}");
    }

    #[test]
    fn validate_code_ref_rejects_windows_backslash_drive_path() {
        // Already caught by the backslash check — verify it stays rejected.
        let cr = CodeRef {
            file: "C:\\x.rs".into(),
            symbol: None,
        };
        assert!(validate_code_ref(&cr).is_err());
    }

    #[test]
    fn validate_code_ref_accepts_colon_in_later_segment() {
        // A `:` in a non-first segment is legal on Unix filesystems.
        let cr = CodeRef {
            file: "src/a:b.rs".into(),
            symbol: None,
        };
        assert!(validate_code_ref(&cr).is_ok());
    }

    // ── validate_code_refs (slice-level count limit) ────────────────────

    #[test]
    fn validate_code_refs_accepts_within_limit() {
        let refs: Vec<_> = (0..limits::MAX_CODE_REFS)
            .map(|i| CodeRef {
                file: format!("src/f{i}.rs"),
                symbol: None,
            })
            .collect();
        assert!(validate_code_refs(&refs).is_ok());
    }

    #[test]
    fn validate_code_refs_rejects_too_many() {
        let refs: Vec<_> = (0..=limits::MAX_CODE_REFS)
            .map(|i| CodeRef {
                file: format!("src/f{i}.rs"),
                symbol: None,
            })
            .collect();
        let err = validate_code_refs(&refs).unwrap_err().to_string();
        assert!(err.contains("too many"), "{err}");
    }

    #[test]
    fn validate_code_refs_propagates_per_ref_error() {
        let refs = vec![CodeRef {
            file: "/absolute.rs".into(),
            symbol: None,
        }];
        let err = validate_code_refs(&refs).unwrap_err().to_string();
        assert!(err.contains("relative"), "{err}");
    }
}
