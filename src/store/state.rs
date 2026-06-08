use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use crate::{Error, Result};

use super::graph::{InMemoryGraph, Issue};
use super::schema::{ComponentFile, DecisionFile, GraphIndex, PatternFile, ProjectFile};

// ── ProjectState ─────────────────────────────────────────────────────────────

/// Complete in-memory snapshot of `.trurl/`.
/// Keyed by filename stem (e.g. `"auth"`, `"error-strategy"`).
pub struct ProjectState {
    pub project: ProjectFile,
    pub components: BTreeMap<String, ComponentFile>,
    pub decisions: BTreeMap<String, DecisionFile>,
    pub patterns: BTreeMap<String, PatternFile>,
    pub graph_index: GraphIndex,
    /// Cached in-memory graph, built at construction. Reflects the state
    /// of the other fields at that point. Call [`rebuild_graph`] after
    /// mutating `graph_index`, `components`, `decisions`, or `patterns`.
    pub graph: InMemoryGraph,
}

impl ProjectState {
    /// Construct with a pre-built graph cache.
    pub fn new(
        project: ProjectFile,
        components: BTreeMap<String, ComponentFile>,
        decisions: BTreeMap<String, DecisionFile>,
        patterns: BTreeMap<String, PatternFile>,
        graph_index: GraphIndex,
    ) -> Self {
        let graph = Self::build_graph_from(&graph_index, &components, &decisions, &patterns);
        Self {
            project,
            components,
            decisions,
            patterns,
            graph_index,
            graph,
        }
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
        components: &BTreeMap<String, ComponentFile>,
        decisions: &BTreeMap<String, DecisionFile>,
        patterns: &BTreeMap<String, PatternFile>,
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
        use crate::store::schema::*;
        use chrono::Utc;
        ProjectState::new(
            ProjectFile {
                trurl_version: FORMAT_VERSION.into(),
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
                rebuilt: Utc::now(),
                nodes: vec![],
                edges: vec![],
            },
        )
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
        state
            .decisions
            .insert("use-redis".into(), sample_decision("use-redis", "project"));
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
                .insert(name.to_string(), sample_decision(name, "project"));
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
            .insert("auth".into(), sample_component("auth"));
        // "auth" is taken by a component — stem must be suffixed.
        assert_eq!(unique_decision_stem(&state, "auth").unwrap(), "auth-2");
    }

    #[test]
    fn unique_stem_avoids_pattern_collision() {
        let mut state = empty_state();
        state.patterns.insert(
            "state-in-redis".into(),
            crate::store::schema::PatternFile {
                pattern: crate::store::schema::Pattern {
                    name: "state-in-redis".into(),
                    description: "test".into(),
                },
            },
        );
        assert_eq!(
            unique_decision_stem(&state, "state-in-redis").unwrap(),
            "state-in-redis-2"
        );
    }

    #[test]
    fn is_node_name_taken_across_types() {
        let mut state = empty_state();
        assert!(state.is_node_name_taken("project"), "reserved");
        assert!(!state.is_node_name_taken("auth"));

        state
            .components
            .insert("auth".into(), sample_component("auth"));
        assert!(state.is_node_name_taken("auth"), "component");
        assert!(!state.is_node_name_taken("use-jwt"));

        state
            .decisions
            .insert("use-jwt".into(), sample_decision("use-jwt", "auth"));
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
