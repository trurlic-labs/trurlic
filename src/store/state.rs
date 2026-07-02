use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::{Error, Result};

use super::graph::{InMemoryGraph, Issue};
use super::schema::{
    ComponentFile, DecisionFile, EdgeEntry, GraphIndex, NodeEntry, PatternFile, ProjectFile,
};

// ── ProjectState ─────────────────────────────────────────────────────────────

/// Complete in-memory snapshot of `.trurlic/`.
/// Keyed by filename stem (e.g. `"auth"`, `"error-strategy"`).
///
/// Content values (`ComponentFile`, `DecisionFile`, `PatternFile`) are
/// `Arc`-wrapped for zero-cost sharing with [`InMemoryGraph`]. Graph
/// rebuilds (which happen on every write operation) clone only the `Arc`
/// pointer — not the underlying data — making rebuild cost O(n) pointer
/// increments instead of O(n) deep copies.
pub struct ProjectState {
    pub project: ProjectFile,
    pub components: BTreeMap<String, Arc<ComponentFile>>,
    pub decisions: BTreeMap<String, Arc<DecisionFile>>,
    pub patterns: BTreeMap<String, Arc<PatternFile>>,
    pub graph_index: GraphIndex,
    /// Absolute path to the project directory (the parent of `.trurlic/`).
    /// Decision `code_refs` are resolved against this root to detect
    /// references to deleted files. [`Store::load_state`] fills it in;
    /// states built directly by tests default to an empty path (which,
    /// with no `code_refs`, never triggers staleness).
    pub(crate) project_root: PathBuf,
    /// Cached in-memory graph. Kept in sync by
    /// [`Store::commit_with_graph`], which assigns the validated graph
    /// on successful commit. Writable only from within `store/`.
    pub(super) graph: InMemoryGraph,
}

impl ProjectState {
    /// Construct from `Arc`-wrapped content maps.
    ///
    /// Content values are `Arc`-wrapped so `InMemoryGraph` can share them
    /// via pointer clone instead of deep copy. Callers that load from disk
    /// wrap at construction time; test helpers like `sample_component` and
    /// `sample_decision` return `Arc<T>` directly.
    pub fn new(
        project: ProjectFile,
        components: BTreeMap<String, Arc<ComponentFile>>,
        decisions: BTreeMap<String, Arc<DecisionFile>>,
        patterns: BTreeMap<String, Arc<PatternFile>>,
        graph_index: GraphIndex,
    ) -> Self {
        let graph = Self::build_graph_from(&graph_index, &components, &decisions, &patterns);
        Self {
            project,
            components,
            decisions,
            patterns,
            graph_index,
            project_root: PathBuf::new(),
            graph,
        }
    }

    /// Read-only access to the cached in-memory graph.
    #[must_use]
    pub fn graph(&self) -> &InMemoryGraph {
        &self.graph
    }

    /// Validate against the cached graph. Only valid on freshly-loaded state;
    /// write paths use [`build_graph`] for post-mutation validation.
    pub fn validate(&self) -> Vec<Issue> {
        self.graph.validate()
    }

    /// Build a fresh [`InMemoryGraph`] from the current (potentially mutated)
    /// state. Used by write paths that need validation after in-memory
    /// mutations — the cached `graph` field may be stale at that point.
    pub fn build_graph(&self) -> InMemoryGraph {
        Self::build_graph_from(
            &self.graph_index,
            &self.components,
            &self.decisions,
            &self.patterns,
        )
    }

    /// Refresh the cached graph to match the current field values.
    ///
    /// Production write paths use [`Store::commit_with_graph`] which
    /// assigns the validated graph directly. This method remains for
    /// test helpers that mutate `graph_index` without going through
    /// the commit pipeline.
    #[cfg(test)]
    pub fn rebuild_graph(&mut self) {
        self.graph = Self::build_graph_from(
            &self.graph_index,
            &self.components,
            &self.decisions,
            &self.patterns,
        );
    }

    fn build_graph_from(
        graph_index: &GraphIndex,
        components: &BTreeMap<String, Arc<ComponentFile>>,
        decisions: &BTreeMap<String, Arc<DecisionFile>>,
        patterns: &BTreeMap<String, Arc<PatternFile>>,
    ) -> InMemoryGraph {
        InMemoryGraph::build(graph_index, components, decisions, patterns)
    }

    /// Check if a name is in use by any node type (component, decision,
    /// pattern, or virtual). Prevents cross-type collisions that would
    /// produce confusing graph integrity errors during commit.
    #[must_use]
    pub fn is_node_name_taken(&self, name: &str) -> bool {
        is_reserved_node_name(name)
            || self.components.contains_key(name)
            || self.decisions.contains_key(name)
            || self.patterns.contains_key(name)
    }

    // ── Graph mutation helpers ───────────────────────────────────────────

    /// Checkpoint the graph index for append-only rollback.
    ///
    /// O(1) — captures only the current `Vec` lengths. On rollback,
    /// nodes and edges are truncated to these lengths, undoing any
    /// pushes that happened after the checkpoint.
    ///
    /// Use [`rollback_graph`](Self::rollback_graph) on commit failure.
    pub(super) fn graph_checkpoint(&self) -> GraphCheckpoint {
        GraphCheckpoint {
            nodes_len: self.graph_index.nodes.len(),
            edges_len: self.graph_index.edges.len(),
        }
    }

    /// Roll back appended nodes and edges to a checkpoint.
    pub(super) fn rollback_graph(&mut self, cp: GraphCheckpoint) {
        self.graph_index.nodes.truncate(cp.nodes_len);
        self.graph_index.edges.truncate(cp.edges_len);
    }

    /// Remove a named node and all incident edges from the graph index.
    ///
    /// Returns the removed items so they can be restored on commit
    /// failure via [`restore_graph_node`](Self::restore_graph_node).
    /// Clones only the affected items (typically 1 node + a few edges)
    /// instead of the entire index.
    pub(super) fn remove_graph_node(&mut self, name: &str) -> RemovedGraphNode {
        let nodes: Vec<NodeEntry> = self
            .graph_index
            .nodes
            .iter()
            .filter(|n| n.name == name)
            .cloned()
            .collect();
        let edges: Vec<EdgeEntry> = self
            .graph_index
            .edges
            .iter()
            .filter(|e| e.from == name || e.to == name)
            .cloned()
            .collect();
        self.graph_index.nodes.retain(|n| n.name != name);
        self.graph_index
            .edges
            .retain(|e| e.from != name && e.to != name);
        RemovedGraphNode { nodes, edges }
    }

    /// Restore a previously removed node and its edges.
    pub(super) fn restore_graph_node(&mut self, removed: RemovedGraphNode) {
        self.graph_index.nodes.extend(removed.nodes);
        self.graph_index.edges.extend(removed.edges);
    }

    /// Update the hash of a named node. Returns the previous hash
    /// so the caller can restore it on commit failure.
    pub(super) fn update_node_hash(&mut self, name: &str, new_hash: String) -> Option<String> {
        self.graph_index
            .nodes
            .iter_mut()
            .find(|n| n.name == name)
            .map(|node| std::mem::replace(&mut node.hash, new_hash))
    }
}

// ── Graph rollback types ────────────────────────────────────────────────────

/// Checkpoint for append-only graph mutations.
///
/// Captures the current `Vec` lengths of nodes and edges. Truncation
/// on rollback is O(K) where K is the number of appended items —
/// compared to O(N) for a full `GraphIndex::clone`.
#[must_use = "discard explicitly with drop() if rollback is not needed"]
pub(super) struct GraphCheckpoint {
    nodes_len: usize,
    edges_len: usize,
}

/// Saved state from a [`ProjectState::remove_graph_node`] call.
///
/// Holds the removed nodes and edges so they can be restored if the
/// commit fails. Clones only the affected items (typically 1 node +
/// 2–5 edges), not the entire graph index.
#[must_use = "discard explicitly with drop() if rollback is not needed"]
pub(super) struct RemovedGraphNode {
    nodes: Vec<NodeEntry>,
    edges: Vec<EdgeEntry>,
}

// ── Name validation ─────────────────────────────────────────────────────────

/// Check whether a name is valid kebab-case.
/// Rules: non-empty, lowercase ASCII letters + digits + hyphens only,
/// no leading/trailing/consecutive hyphens.
#[must_use]
pub fn is_valid_kebab_case(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Names reserved for internal graph nodes. These cannot be used as
/// component, decision, or pattern identifiers because they would collide
/// with virtual nodes in the graph index.
#[must_use]
pub fn is_reserved_node_name(name: &str) -> bool {
    name == "project"
}

/// Reject ASCII control characters that could corrupt TOML files or
/// produce surprising on-disk content. Allows common whitespace
/// (newline, carriage return, tab).
///
/// Shared across all mutation entry points (MCP, map REST API, CLI) so
/// every path enforces the same input hygiene.
#[must_use]
pub fn has_control_chars(s: &str) -> bool {
    s.bytes()
        .any(|b| b < 0x20 && b != b'\n' && b != b'\r' && b != b'\t')
}

// ── Slugify ─────────────────────────────────────────────────────────────────

const MAX_SLUG_LEN: usize = 60;

/// Convert a free-text string to a kebab-case slug suitable for
/// filenames and node names.
///
/// Lowercases ASCII letters, keeps digits, replaces everything else
/// with hyphens. Collapses runs, strips leading/trailing hyphens,
/// truncates at a word boundary, and falls back to `"decision"` for
/// empty input.
#[must_use]
pub fn slugify(input: &str) -> String {
    let mut slug = String::with_capacity(input.len());
    let mut prev_hyphen = true;

    for c in input.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
            prev_hyphen = false;
        } else if !prev_hyphen {
            slug.push('-');
            prev_hyphen = true;
        }
    }

    while slug.ends_with('-') {
        slug.pop();
    }

    if slug.len() > MAX_SLUG_LEN {
        slug.truncate(MAX_SLUG_LEN);
        if let Some(last_hyphen) = slug.rfind('-') {
            slug.truncate(last_hyphen);
        }
        while slug.ends_with('-') {
            slug.pop();
        }
    }

    if slug.is_empty() {
        slug.push_str("decision");
    }

    slug
}

// ── Unique decision stem ────────────────────────────────────────────────────

const MAX_DEDUP_SUFFIX: u32 = 10_000;

/// Derive a unique decision filename stem from `base`, appending `-2`,
/// `-3`, … if `base` collides with any existing node. Checks components,
/// decisions, patterns, and reserved names to prevent cross-type
/// collisions that would produce confusing graph integrity errors.
pub fn unique_decision_stem(state: &ProjectState, base: &str) -> Result<String> {
    if is_reserved_node_name(base) {
        let candidate = format!("{base}-decision");
        return unique_decision_stem(state, &candidate);
    }
    if !state.is_node_name_taken(base) {
        return Ok(base.to_string());
    }
    for n in 2..=MAX_DEDUP_SUFFIX {
        let candidate = format!("{base}-{n}");
        if !state.is_node_name_taken(&candidate) {
            return Ok(candidate);
        }
    }
    Err(Error::Validation(format!(
        "too many nodes with stem `{base}` (limit: {MAX_DEDUP_SUFFIX})"
    )))
}

// ── Directory listing ───────────────────────────────────────────────────────

pub(super) fn list_toml_stems(dir: &Path) -> Result<Vec<String>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(Error::Io(e)),
    };

    let mut names = Vec::new();
    for entry in entries {
        let path = entry?.path();
        let is_toml = path.extension().is_some_and(|ext| ext == "toml");
        if is_toml && let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            names.push(stem.to_string());
        }
    }
    names.sort_unstable();
    Ok(names)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::graph::Severity;

    use crate::store::testing::*;

    use tempfile::TempDir;

    // ── is_valid_kebab_case ──────────────────────────────────────────────

    #[test]
    fn kebab_valid_names() {
        assert!(is_valid_kebab_case("auth"));
        assert!(is_valid_kebab_case("rate-limiter"));
        assert!(is_valid_kebab_case("mcp-server"));
        assert!(is_valid_kebab_case("a"));
        assert!(is_valid_kebab_case("component1"));
        assert!(is_valid_kebab_case("my-app-v2"));
    }

    #[test]
    fn kebab_rejects_invalid() {
        assert!(!is_valid_kebab_case(""));
        assert!(!is_valid_kebab_case("-leading"));
        assert!(!is_valid_kebab_case("trailing-"));
        assert!(!is_valid_kebab_case("double--hyphen"));
        assert!(!is_valid_kebab_case("UpperCase"));
        assert!(!is_valid_kebab_case("has_underscore"));
        assert!(!is_valid_kebab_case("has space"));
        assert!(!is_valid_kebab_case("has.dot"));
        assert!(!is_valid_kebab_case("special!char"));
    }

    // ── is_reserved_node_name ───────────────────────────────────────────

    #[test]
    fn reserved_names() {
        assert!(is_reserved_node_name("project"));
        assert!(!is_reserved_node_name("my-project"));
        assert!(!is_reserved_node_name("auth"));
        assert!(!is_reserved_node_name(""));
    }

    // ── slugify ─────────────────────────────────────────────────────────

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("Use Redis"), "use-redis");
        assert_eq!(slugify("JWT with DPoP binding"), "jwt-with-dpop-binding");
    }

    #[test]
    fn slugify_special_chars() {
        assert_eq!(slugify("Result<T, AppError>"), "result-t-apperror");
        assert_eq!(
            slugify("429 + retry-after header"),
            "429-retry-after-header"
        );
    }

    #[test]
    fn slugify_collapses_runs() {
        assert_eq!(slugify("one   two---three"), "one-two-three");
        assert_eq!(slugify("---leading"), "leading");
        assert_eq!(slugify("trailing---"), "trailing");
    }

    #[test]
    fn slugify_truncates_at_word_boundary() {
        let long = "a ".repeat(100);
        let slug = slugify(&long);
        assert!(slug.len() <= MAX_SLUG_LEN);
        assert!(!slug.ends_with('-'));
    }

    #[test]
    fn slugify_empty_input() {
        assert_eq!(slugify(""), "decision");
        assert_eq!(slugify("!!!"), "decision");
    }

    // ── unique_decision_stem ────────────────────────────────────────────

    fn empty_state() -> ProjectState {
        crate::store::testing::empty_project_state()
    }

    #[test]
    fn unique_stem_no_collision() {
        let state = empty_state();
        assert_eq!(
            unique_decision_stem(&state, "use-redis").unwrap(),
            "use-redis"
        );
    }

    #[test]
    fn unique_stem_appends_suffix_on_collision() {
        let mut state = empty_state();
        state.decisions.insert(
            "use-redis".into(),
            sample_decision("use-redis", "project").into(),
        );
        assert_eq!(
            unique_decision_stem(&state, "use-redis").unwrap(),
            "use-redis-2"
        );
    }

    #[test]
    fn unique_stem_skips_taken_suffixes() {
        let mut state = empty_state();
        for name in &["use-redis", "use-redis-2", "use-redis-3"] {
            state
                .decisions
                .insert(name.to_string(), sample_decision(name, "project").into());
        }
        assert_eq!(
            unique_decision_stem(&state, "use-redis").unwrap(),
            "use-redis-4"
        );
    }

    #[test]
    fn unique_stem_disambiguates_reserved_name() {
        let state = empty_state();
        let stem = unique_decision_stem(&state, "project").unwrap();
        assert_ne!(stem, "project", "reserved name must be disambiguated");
        assert!(
            stem.starts_with("project-"),
            "disambiguated stem should keep the prefix: {stem}"
        );
    }

    #[test]
    fn unique_stem_avoids_component_collision() {
        let mut state = empty_state();
        state
            .components
            .insert("auth".into(), sample_component("auth").into());
        // "auth" is taken by a component — stem must be suffixed.
        assert_eq!(unique_decision_stem(&state, "auth").unwrap(), "auth-2");
    }

    #[test]
    fn unique_stem_avoids_pattern_collision() {
        let mut state = empty_state();
        state.patterns.insert(
            "state-in-redis".into(),
            Arc::new(crate::store::schema::PatternFile {
                pattern: crate::store::schema::Pattern {
                    name: "state-in-redis".into(),
                    description: "test".into(),
                },
            }),
        );
        assert_eq!(
            unique_decision_stem(&state, "state-in-redis").unwrap(),
            "state-in-redis-2"
        );
    }

    // ── has_control_chars ─────────────────────────────────────────────

    #[test]
    fn control_chars_rejects_nul() {
        assert!(has_control_chars("hello\x00world"));
    }

    #[test]
    fn control_chars_rejects_bell() {
        assert!(has_control_chars("alert\x07"));
    }

    #[test]
    fn control_chars_rejects_escape() {
        assert!(has_control_chars("\x1b[31m red"));
    }

    #[test]
    fn control_chars_allows_normal_whitespace() {
        assert!(!has_control_chars("line\nnext\r\nand\ttab"));
    }

    #[test]
    fn control_chars_allows_plain_text() {
        assert!(!has_control_chars("JWT with DPoP binding, 15min lease"));
    }

    #[test]
    fn control_chars_allows_empty() {
        assert!(!has_control_chars(""));
    }

    #[test]
    fn control_chars_allows_unicode() {
        assert!(!has_control_chars("café — naïve résumé"));
    }

    #[test]
    fn is_node_name_taken_across_types() {
        let mut state = empty_state();
        assert!(state.is_node_name_taken("project"), "reserved");
        assert!(!state.is_node_name_taken("auth"));

        state
            .components
            .insert("auth".into(), sample_component("auth").into());
        assert!(state.is_node_name_taken("auth"), "component");
        assert!(!state.is_node_name_taken("use-jwt"));

        state
            .decisions
            .insert("use-jwt".into(), sample_decision("use-jwt", "auth").into());
        assert!(state.is_node_name_taken("use-jwt"), "decision");
    }

    // ── validate ─────────────────────────────────────────────────────────

    #[test]
    fn validate_clean_state() {
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

        let dec = sample_decision("token-format", "auth");
        store
            .write_atomic(&lock, &store.decision_path("token-format"), &dec)
            .unwrap();

        let state = store.load_state().unwrap();
        assert!(state.validate().is_empty());
    }

    #[test]
    fn validate_catches_dangling_decision_component() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let dec = sample_decision("stale-decision", "deleted-component");
        store
            .write_atomic(&lock, &store.decision_path("stale-decision"), &dec)
            .unwrap();

        let state = store.load_state().unwrap();
        let issues = state.validate();
        assert!(issues.iter().any(
            |i| i.message.contains("deleted-component") && i.message.contains("does not exist")
        ));
    }

    #[test]
    fn validate_allows_project_wide_decisions() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let dec = sample_decision("error-strategy", "project");
        store
            .write_atomic(&lock, &store.decision_path("error-strategy"), &dec)
            .unwrap();

        let state = store.load_state().unwrap();
        assert!(state.validate().is_empty());
    }

    #[test]
    fn validate_catches_filename_mismatch() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let comp = sample_component("wrong-name");
        store
            .write_atomic(&lock, &store.component_path("actual-file"), &comp)
            .unwrap();

        let state = store.load_state().unwrap();
        let issues = state.validate();
        assert!(
            issues
                .iter()
                .any(|i| i.message.contains("actual-file") && i.message.contains("wrong-name"))
        );
    }

    #[test]
    fn validate_catches_non_kebab_component_name() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let mut comp = sample_component("bad-name");
        comp.component.name = "Bad_Name".into();
        store
            .write_atomic(&lock, &store.component_path("Bad_Name"), &comp)
            .unwrap();

        let state = store.load_state().unwrap();
        let issues = state.validate();
        assert!(issues.iter().any(|i| i.message.contains("kebab-case")));
    }

    #[test]
    fn validate_catches_empty_decision_choice() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let mut dec = sample_decision("bad-decision", "project");
        dec.decision.choice = String::new();
        store
            .write_atomic(&lock, &store.decision_path("bad-decision"), &dec)
            .unwrap();

        let state = store.load_state().unwrap();
        let issues = state.validate();
        assert!(issues.iter().any(|i| i.message.contains("empty choice")));
    }

    #[test]
    fn validate_catches_whitespace_only_reason() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let mut dec = sample_decision("bad-decision", "project");
        dec.decision.reason = "   ".into();
        store
            .write_atomic(&lock, &store.decision_path("bad-decision"), &dec)
            .unwrap();

        let state = store.load_state().unwrap();
        let issues = state.validate();
        assert!(issues.iter().any(|i| i.message.contains("empty reason")));
    }

    #[test]
    fn validate_catches_non_kebab_decision_component() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let mut dec = sample_decision("bad-ref", "project");
        dec.decision.component = "Not Kebab".into();
        store
            .write_atomic(&lock, &store.decision_path("bad-ref"), &dec)
            .unwrap();

        let state = store.load_state().unwrap();
        let issues = state.validate();
        assert!(
            issues
                .iter()
                .any(|i| i.message.contains("invalid component"))
        );
    }

    #[test]
    fn validate_catches_empty_pattern_description() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        let pat = crate::store::schema::PatternFile {
            pattern: crate::store::schema::Pattern {
                name: "empty-pat".into(),
                description: "   ".into(),
            },
        };
        store
            .write_atomic(&lock, &store.pattern_path("empty-pat"), &pat)
            .unwrap();

        let state = store.load_state().unwrap();
        let issues = state.validate();
        assert!(
            issues
                .iter()
                .any(|i| i.message.contains("empty description"))
        );
    }

    #[test]
    fn validate_reports_severity() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let lock = store.lock().unwrap();

        // Empty description is a Warning; empty choice is an Error.
        let pat = crate::store::schema::PatternFile {
            pattern: crate::store::schema::Pattern {
                name: "test-pat".into(),
                description: "   ".into(),
            },
        };
        store
            .write_atomic(&lock, &store.pattern_path("test-pat"), &pat)
            .unwrap();

        let mut dec = sample_decision("bad-dec", "project");
        dec.decision.choice = String::new();
        store
            .write_atomic(&lock, &store.decision_path("bad-dec"), &dec)
            .unwrap();

        let state = store.load_state().unwrap();
        let issues = state.validate();
        assert!(issues
            .iter()
            .any(|i| i.severity == Severity::Warning && i.message.contains("empty description")));
        assert!(
            issues
                .iter()
                .any(|i| i.severity == Severity::Error && i.message.contains("empty choice"))
        );
    }
}
