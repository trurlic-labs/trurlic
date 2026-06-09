use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use chrono::Utc;

use super::schema::{
    ComponentFile, DecisionFile, EdgeEntry, EdgeKind, GraphIndex, NodeEntry, NodeKind, PatternFile,
};

// ── Public types ────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Edge {
    pub target: Arc<str>,
    pub kind: EdgeKind,
}

#[derive(Debug, Clone)]
pub struct NodeMeta {
    pub kind: NodeKind,
    pub tags: Vec<Arc<str>>,
    pub hash: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// This node is the edge source.
    Forward,
    /// This node is the edge target.
    Reverse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone)]
pub struct Issue {
    pub severity: Severity,
    pub message: String,
    pub node: Option<String>,
}

/// A hard blocker that prevents node removal.
#[derive(Debug, Clone)]
pub struct CascadeBlocker {
    /// The node that causes the block.
    pub node: String,
    /// The edge kind creating the dependency.
    pub edge: EdgeKind,
    /// Human-readable explanation.
    pub message: String,
}

/// A non-blocking side-effect the user should know about.
#[derive(Debug, Clone)]
pub struct CascadeEffect {
    /// The affected node.
    pub node: String,
    /// The edge kind being removed or broken.
    pub edge: EdgeKind,
    /// Human-readable explanation.
    pub message: String,
}

/// An outgoing edge from the deleted node that will be silently removed.
#[derive(Debug, Clone)]
pub struct CascadeCleanup {
    /// The edge kind being removed.
    pub edge: EdgeKind,
    /// The target node of the edge.
    pub target: String,
}

/// Result of a cascade pre-flight check before removing a node.
///
/// Shared by CLI, MCP, and map API to enforce identical deletion rules
/// regardless of the mutation entry point.
#[derive(Debug, Clone)]
pub struct CascadeResult {
    /// Non-empty means removal is blocked.
    pub blockers: Vec<CascadeBlocker>,
    /// Non-blocking side-effects the user should be aware of.
    pub warnings: Vec<CascadeEffect>,
    /// Outgoing edges that will be silently cleaned up.
    pub cleanups: Vec<CascadeCleanup>,
}

impl CascadeResult {
    #[must_use]
    pub fn is_blocked(&self) -> bool {
        !self.blockers.is_empty()
    }

    /// Concatenate blocker messages for flat error reporting (CLI, map 409).
    #[must_use]
    pub fn blocker_summary(&self) -> String {
        self.blockers
            .iter()
            .map(|b| b.message.as_str())
            .collect::<Vec<_>>()
            .join("; ")
    }
}

// ── InMemoryGraph ───────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct InMemoryGraph {
    pub(crate) nodes: HashMap<Arc<str>, NodeMeta>,
    pub(crate) forward: HashMap<Arc<str>, Vec<Edge>>,
    pub(crate) reverse: HashMap<Arc<str>, Vec<Edge>>,
    pub(crate) components: HashMap<Arc<str>, Arc<ComponentFile>>,
    pub(crate) decisions: HashMap<Arc<str>, Arc<DecisionFile>>,
    pub(crate) patterns: HashMap<Arc<str>, Arc<PatternFile>>,
}

impl InMemoryGraph {
    /// Build from a parsed [`GraphIndex`] and content maps.
    ///
    /// All node names are interned as [`Arc<str>`] for zero-cost sharing
    /// across adjacency maps, content caches, and query results.
    /// Content values are `Arc::clone`'d — pointer increment, not deep copy.
    #[must_use]
    pub fn build(
        index: &GraphIndex,
        components: &BTreeMap<String, Arc<ComponentFile>>,
        decisions: &BTreeMap<String, Arc<DecisionFile>>,
        patterns: &BTreeMap<String, Arc<PatternFile>>,
    ) -> Self {
        // Intern every name that appears in nodes or edges.
        let mut pool: HashMap<String, Arc<str>> = HashMap::with_capacity(index.nodes.len());
        for node in &index.nodes {
            pool.entry(node.name.clone())
                .or_insert_with(|| Arc::from(node.name.as_str()));
        }
        for edge in &index.edges {
            pool.entry(edge.from.clone())
                .or_insert_with(|| Arc::from(edge.from.as_str()));
            pool.entry(edge.to.clone())
                .or_insert_with(|| Arc::from(edge.to.as_str()));
        }
        let intern =
            |name: &str| -> Arc<str> { pool.get(name).cloned().unwrap_or_else(|| Arc::from(name)) };

        let mut nodes = HashMap::with_capacity(index.nodes.len());
        for node in &index.nodes {
            nodes.insert(
                intern(&node.name),
                NodeMeta {
                    kind: node.kind,
                    tags: node.tags.iter().map(|t| Arc::from(t.as_str())).collect(),
                    hash: node.hash.clone(),
                },
            );
        }

        let mut forward: HashMap<Arc<str>, Vec<Edge>> = HashMap::new();
        let mut reverse: HashMap<Arc<str>, Vec<Edge>> = HashMap::new();
        for edge in &index.edges {
            let from = intern(&edge.from);
            let to = intern(&edge.to);
            forward.entry(from.clone()).or_default().push(Edge {
                target: to.clone(),
                kind: edge.kind,
            });
            reverse.entry(to).or_default().push(Edge {
                target: from,
                kind: edge.kind,
            });
        }

        Self {
            nodes,
            forward,
            reverse,
            components: components
                .iter()
                .map(|(k, v)| (intern(k), Arc::clone(v)))
                .collect(),
            decisions: decisions
                .iter()
                .map(|(k, v)| (intern(k), Arc::clone(v)))
                .collect(),
            patterns: patterns
                .iter()
                .map(|(k, v)| (intern(k), Arc::clone(v)))
                .collect(),
        }
    }

    // ── Queries ──────────────────────────────────────────────────────────

    /// All decisions belonging to a component (reverse `BelongsTo` edges).
    pub fn decisions_for(&self, component: &str) -> Vec<(&Arc<str>, &DecisionFile)> {
        self.reverse_targets(component, EdgeKind::BelongsTo, |name| {
            self.decisions.get(name).map(|d| (name, d.as_ref()))
        })
    }

    /// All project-wide decisions (`BelongsTo "project"`).
    pub fn project_decisions(&self) -> Vec<(&Arc<str>, &DecisionFile)> {
        self.decisions_for("project")
    }

    /// Components this component connects to (forward `ConnectsTo`).
    pub fn connects_to(&self, component: &str) -> Vec<&str> {
        self.forward_targets(component, EdgeKind::ConnectsTo)
    }

    /// Components that connect to this one (reverse `ConnectsTo`).
    pub fn connects_from(&self, component: &str) -> Vec<&str> {
        self.reverse
            .get(component)
            .map(|edges| {
                edges
                    .iter()
                    .filter(|e| e.kind == EdgeKind::ConnectsTo)
                    .map(|e| e.target.as_ref())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Decisions from directly connected components (both directions, depth 1).
    pub fn related_decisions(&self, component: &str) -> Vec<(&Arc<str>, &DecisionFile)> {
        let mut connected: HashSet<&str> = HashSet::new();
        for arc in self.connects_to(component) {
            connected.insert(arc);
        }
        for arc in self.connects_from(component) {
            connected.insert(arc);
        }

        let mut seen: HashSet<&str> = HashSet::new();
        let mut result = Vec::new();
        for conn in connected {
            for (name, dec) in self.decisions_for(conn) {
                if seen.insert(name) {
                    result.push((name, dec));
                }
            }
        }
        result
    }

    /// Patterns that apply to a component (reverse `AppliesTo` edges).
    pub fn patterns_for(&self, component: &str) -> Vec<(&Arc<str>, &PatternFile)> {
        self.reverse_targets(component, EdgeKind::AppliesTo, |name| {
            self.patterns.get(name).map(|p| (name, p.as_ref()))
        })
    }

    /// Whether this decision is a member of any pattern (has incoming `MemberOf` edge).
    pub fn is_pattern_member(&self, decision: &str) -> bool {
        self.reverse
            .get(decision)
            .is_some_and(|edges| edges.iter().any(|e| e.kind == EdgeKind::MemberOf))
    }

    /// Patterns that include this decision (reverse `MemberOf` edges).
    pub fn patterns_containing(&self, decision: &str) -> Vec<(&Arc<str>, &PatternFile)> {
        self.reverse_targets(decision, EdgeKind::MemberOf, |name| {
            self.patterns.get(name).map(|p| (name, p.as_ref()))
        })
    }

    /// Decisions that are members of a pattern (forward `MemberOf` edges).
    pub fn decisions_for_pattern(&self, pattern: &str) -> Vec<(&Arc<str>, &DecisionFile)> {
        self.forward
            .get(pattern)
            .map(|edges| {
                edges
                    .iter()
                    .filter(|e| e.kind == EdgeKind::MemberOf)
                    .filter_map(|e| {
                        self.decisions
                            .get(&e.target)
                            .map(|d| (&e.target, d.as_ref()))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// BFS on `DependsOn` forward edges from a set of seed decisions.
    ///
    /// Returns all reachable decisions within `max_depth` hops, excluding
    /// the seeds themselves. Used by `get_context` to surface transitive
    /// constraints that affect a component's decisions — e.g. if auth's
    /// "use-jwt" depends on infrastructure's "redis-available", that
    /// constraint appears in the auth context even if infrastructure is
    /// not directly connected to auth.
    ///
    /// Cycles are handled by the visited set — each node is visited at
    /// most once.
    pub fn transitive_depends_on(
        &self,
        seeds: &[&str],
        max_depth: usize,
    ) -> Vec<(&Arc<str>, &DecisionFile)> {
        use std::collections::VecDeque;

        let mut visited: HashSet<&str> = HashSet::with_capacity(seeds.len() * 4);
        for &seed in seeds {
            visited.insert(seed);
        }

        let mut queue: VecDeque<(&str, usize)> = VecDeque::new();

        // Seed BFS from all seed decisions.
        for &seed in seeds {
            if let Some(edges) = self.forward.get(seed) {
                for edge in edges {
                    if edge.kind == EdgeKind::DependsOn && visited.insert(edge.target.as_ref()) {
                        queue.push_back((edge.target.as_ref(), 1));
                    }
                }
            }
        }

        let mut result = Vec::new();

        while let Some((node, depth)) = queue.pop_front() {
            if let Some((key, dec)) = self.decisions.get_key_value(node) {
                result.push((key, dec.as_ref()));
            }
            if depth < max_depth
                && let Some(edges) = self.forward.get(node)
            {
                for edge in edges {
                    if edge.kind == EdgeKind::DependsOn && visited.insert(edge.target.as_ref()) {
                        queue.push_back((edge.target.as_ref(), depth + 1));
                    }
                }
            }
        }

        result
    }

    /// Components that a pattern applies to (forward `AppliesTo` edges).
    pub fn components_for_pattern(&self, pattern: &str) -> Vec<&str> {
        self.forward_targets(pattern, EdgeKind::AppliesTo)
    }

    /// Count forward edges of a specific kind from a node.
    pub fn forward_edge_count(&self, node: &str, kind: EdgeKind) -> usize {
        self.forward
            .get(node)
            .map(|edges| edges.iter().filter(|e| e.kind == kind).count())
            .unwrap_or(0)
    }

    /// All edges involving a node (both directions). For cascade checks.
    ///
    /// Returns `(other_node, edge, direction)` tuples.
    pub fn edges_involving(&self, node: &str) -> Vec<(&str, &Edge, Direction)> {
        let mut result = Vec::new();
        if let Some(edges) = self.forward.get(node) {
            for edge in edges {
                result.push((edge.target.as_ref(), edge, Direction::Forward));
            }
        }
        if let Some(edges) = self.reverse.get(node) {
            for edge in edges {
                result.push((edge.target.as_ref(), edge, Direction::Reverse));
            }
        }
        result
    }

    // ── Cascade checks ──────────────────────────────────────────────────

    /// Pre-flight check for removing a decision. Returns blockers that
    /// must prevent the removal, warnings about side-effects, and cleanups
    /// for outgoing edges that will be silently removed.
    ///
    /// Used by the CLI, MCP server, and map API to enforce identical
    /// cascade rules regardless of the mutation entry point.
    #[must_use]
    pub fn check_decision_cascade(&self, name: &str) -> CascadeResult {
        let involved = self.edges_involving(name);
        let mut blockers = Vec::new();
        let mut warnings = Vec::new();
        let mut cleanups = Vec::new();

        for (other, edge, dir) in &involved {
            match (edge.kind, *dir) {
                // Block: other decisions depend on this one.
                (EdgeKind::DependsOn, Direction::Reverse) => {
                    blockers.push(CascadeBlocker {
                        node: other.to_string(),
                        edge: EdgeKind::DependsOn,
                        message: format!(
                            "decision `{other}` depends on `{name}` — \
                             remove or update it first"
                        ),
                    });
                }
                // Block or warn: pattern membership.
                (EdgeKind::MemberOf, Direction::Reverse) => {
                    let member_count = self.forward_edge_count(other, EdgeKind::MemberOf);
                    if member_count <= 2 {
                        blockers.push(CascadeBlocker {
                            node: other.to_string(),
                            edge: EdgeKind::MemberOf,
                            message: format!(
                                "pattern `{other}` would have fewer than 2 members — \
                                 remove or update the pattern first"
                            ),
                        });
                    } else {
                        warnings.push(CascadeEffect {
                            node: other.to_string(),
                            edge: EdgeKind::MemberOf,
                            message: format!(
                                "pattern `{other}` will be updated to exclude this decision"
                            ),
                        });
                    }
                }
                // Warn: incoming constrains edges.
                (EdgeKind::Constrains, Direction::Reverse) => {
                    warnings.push(CascadeEffect {
                        node: other.to_string(),
                        edge: EdgeKind::Constrains,
                        message: format!("constraint from `{other}` will be removed"),
                    });
                }
                // Warn: broken supersede chains.
                (EdgeKind::Supersedes, Direction::Reverse) => {
                    warnings.push(CascadeEffect {
                        node: other.to_string(),
                        edge: EdgeKind::Supersedes,
                        message: format!("supersede chain broken — `{other}` references `{name}`"),
                    });
                }
                // Clean: all outgoing edges from this decision.
                (_, Direction::Forward) => {
                    cleanups.push(CascadeCleanup {
                        edge: edge.kind,
                        target: other.to_string(),
                    });
                }
                // Other reverse edges: no action needed.
                _ => {}
            }
        }

        CascadeResult {
            blockers,
            warnings,
            cleanups,
        }
    }

    /// Pre-flight check for removing a component. Returns blockers that
    /// must prevent the removal, warnings about side-effects, and cleanups
    /// for outgoing edges that will be silently removed.
    #[must_use]
    pub fn check_component_cascade(&self, name: &str) -> CascadeResult {
        let involved = self.edges_involving(name);
        let mut blockers = Vec::new();
        let mut warnings = Vec::new();
        let mut cleanups = Vec::new();

        for (other, edge, dir) in &involved {
            match (edge.kind, *dir) {
                // Block: decisions belong to this component.
                (EdgeKind::BelongsTo, Direction::Reverse) => {
                    blockers.push(CascadeBlocker {
                        node: other.to_string(),
                        edge: EdgeKind::BelongsTo,
                        message: format!(
                            "decision `{other}` belongs to `{name}` — \
                             remove or reassign it first"
                        ),
                    });
                }
                // Warn: patterns that apply to this component.
                (EdgeKind::AppliesTo, Direction::Reverse) => {
                    warnings.push(CascadeEffect {
                        node: other.to_string(),
                        edge: EdgeKind::AppliesTo,
                        message: format!(
                            "pattern `{other}` applies_to association will be removed"
                        ),
                    });
                }
                // Warn: incoming connections from other components.
                (EdgeKind::ConnectsTo, Direction::Reverse) => {
                    warnings.push(CascadeEffect {
                        node: other.to_string(),
                        edge: EdgeKind::ConnectsTo,
                        message: format!("incoming connection from `{other}` will be removed"),
                    });
                }
                // Clean: all outgoing edges from this component.
                (_, Direction::Forward) => {
                    cleanups.push(CascadeCleanup {
                        edge: edge.kind,
                        target: other.to_string(),
                    });
                }
                // Other reverse edges: no action needed.
                _ => {}
            }
        }

        CascadeResult {
            blockers,
            warnings,
            cleanups,
        }
    }

    // ── Content access ───────────────────────────────────────────────────

    #[cfg(test)]
    pub fn component(&self, name: &str) -> Option<&ComponentFile> {
        self.components.get(name).map(|c| c.as_ref())
    }

    #[cfg(test)]
    pub fn decision(&self, name: &str) -> Option<&DecisionFile> {
        self.decisions.get(name).map(|d| d.as_ref())
    }

    pub fn node_meta(&self, name: &str) -> Option<&NodeMeta> {
        self.nodes.get(name)
    }

    #[cfg(test)]
    pub fn component_count(&self) -> usize {
        self.components.len()
    }

    #[cfg(test)]
    pub fn decision_count(&self) -> usize {
        self.decisions.len()
    }

    #[cfg(test)]
    pub fn pattern_count(&self) -> usize {
        self.patterns.len()
    }

    // ── Serialization ────────────────────────────────────────────────────

    /// Export current state as [`GraphIndex`] (sorted for deterministic output).
    #[must_use]
    pub fn to_index(&self) -> GraphIndex {
        let mut nodes: Vec<NodeEntry> = self
            .nodes
            .iter()
            .map(|(name, meta)| NodeEntry {
                name: name.to_string(),
                kind: meta.kind,
                tags: meta.tags.iter().map(|t| t.to_string()).collect(),
                hash: meta.hash.clone(),
            })
            .collect();
        nodes.sort_by(|a, b| a.name.cmp(&b.name));

        let mut edges: Vec<EdgeEntry> = Vec::new();
        for (from, edge_list) in &self.forward {
            for edge in edge_list {
                edges.push(EdgeEntry {
                    from: from.to_string(),
                    to: edge.target.to_string(),
                    kind: edge.kind,
                });
            }
        }
        edges.sort_by(|a, b| (&a.from, &a.to, &a.kind).cmp(&(&b.from, &b.to, &b.kind)));

        GraphIndex {
            version: 1,
            rebuilt: Utc::now(),
            nodes,
            edges,
        }
    }
}

// ── Private helpers ─────────────────────────────────────────────────────────

impl InMemoryGraph {
    fn forward_targets(&self, node: &str, kind: EdgeKind) -> Vec<&str> {
        self.forward
            .get(node)
            .map(|edges| {
                edges
                    .iter()
                    .filter(|e| e.kind == kind)
                    .map(|e| e.target.as_ref())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Collect transformed reverse edge targets of a specific kind.
    fn reverse_targets<'a, T, F>(&'a self, node: &str, kind: EdgeKind, transform: F) -> Vec<T>
    where
        F: Fn(&'a Arc<str>) -> Option<T>,
    {
        self.reverse
            .get(node)
            .map(|edges| {
                edges
                    .iter()
                    .filter(|e| e.kind == kind)
                    .filter_map(|e| transform(&e.target))
                    .collect()
            })
            .unwrap_or_default()
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::schema::*;
    use chrono::TimeZone;

    // ── Fixtures ─────────────────────────────────────────────────────────

    fn ts() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2025, 6, 1, 10, 0, 0).unwrap()
    }

    /// Wrap a BTreeMap's values in Arc for InMemoryGraph::build().
    fn arc_map<T>(map: BTreeMap<String, T>) -> BTreeMap<String, Arc<T>> {
        map.into_iter().map(|(k, v)| (k, Arc::new(v))).collect()
    }

    /// Realistic graph:
    ///   Components: project (virtual), auth, database, rate-limiter
    ///   Decisions: use-jwt (auth), error-strategy (project), db-pool (database)
    ///   Edges: auth→database (ConnectsTo), rate-limiter→database (ConnectsTo),
    ///          BelongsTo for all decisions
    fn test_graph() -> InMemoryGraph {
        let index = GraphIndex {
            version: 1,
            rebuilt: ts(),
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
                    created: ts(),
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
                    created: ts(),
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
                    created: ts(),
                },
            },
        );

        InMemoryGraph::build(
            &index,
            &arc_map(components),
            &arc_map(decisions),
            &BTreeMap::new(),
        )
    }

    // ── build ────────────────────────────────────────────────────────────

    #[test]
    fn build_populates_nodes() {
        let g = test_graph();
        assert_eq!(g.nodes.len(), 7);
        assert!(g.node_meta("auth").is_some());
        assert_eq!(g.node_meta("auth").unwrap().kind, NodeKind::Component);
        assert_eq!(g.node_meta("use-jwt").unwrap().kind, NodeKind::Decision);
    }

    #[test]
    fn build_preserves_tags() {
        let g = test_graph();
        let meta = g.node_meta("auth").unwrap();
        assert_eq!(meta.tags.len(), 1);
        assert_eq!(meta.tags[0].as_ref(), "security");
    }

    #[test]
    fn build_populates_content() {
        let g = test_graph();
        assert_eq!(g.component_count(), 3);
        assert_eq!(g.decision_count(), 3);
        assert_eq!(g.pattern_count(), 0);
        assert!(g.component("auth").is_some());
        assert!(g.decision("use-jwt").is_some());
        assert!(g.component("nonexistent").is_none());
    }

    // ── decisions_for ────────────────────────────────────────────────────

    #[test]
    fn decisions_for_returns_component_decisions() {
        let g = test_graph();
        let decs = g.decisions_for("auth");
        assert_eq!(decs.len(), 1);
        assert_eq!(decs[0].0.as_ref(), "use-jwt");
    }

    #[test]
    fn decisions_for_empty_when_none() {
        let g = test_graph();
        assert!(g.decisions_for("rate-limiter").is_empty());
    }

    #[test]
    fn project_decisions_returns_project_wide() {
        let g = test_graph();
        let decs = g.project_decisions();
        assert_eq!(decs.len(), 1);
        assert_eq!(decs[0].0.as_ref(), "error-strategy");
    }

    // ── connects_to / connects_from ──────────────────────────────────────

    #[test]
    fn connects_to_returns_forward() {
        let g = test_graph();
        let targets = g.connects_to("auth");
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0], "database");
    }

    #[test]
    fn connects_from_returns_reverse() {
        let g = test_graph();
        let sources = g.connects_from("database");
        assert_eq!(sources.len(), 2);
        let names: HashSet<&str> = sources.iter().copied().collect();
        assert!(names.contains("auth"));
        assert!(names.contains("rate-limiter"));
    }

    #[test]
    fn connects_to_empty_for_leaf() {
        let g = test_graph();
        assert!(g.connects_to("database").is_empty());
    }

    // ── related_decisions ────────────────────────────────────────────────

    #[test]
    fn related_decisions_includes_connected() {
        let g = test_graph();
        // auth → database, so database's decisions are related to auth.
        let related = g.related_decisions("auth");
        assert_eq!(related.len(), 1);
        assert_eq!(related[0].0.as_ref(), "db-pool");
    }

    #[test]
    fn related_decisions_includes_reverse_connected() {
        let g = test_graph();
        // database ← auth, database ← rate-limiter, so auth decisions are related to database.
        let related = g.related_decisions("database");
        let names: HashSet<&str> = related.iter().map(|(n, _)| n.as_ref()).collect();
        assert!(names.contains("use-jwt"));
    }

    // ── patterns_for ─────────────────────────────────────────────────────

    #[test]
    fn patterns_for_returns_matching() {
        let index = GraphIndex {
            version: 1,
            rebuilt: ts(),
            nodes: vec![
                NodeEntry {
                    name: "project".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: "p".into(),
                },
                NodeEntry {
                    name: "comp".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: "c".into(),
                },
                NodeEntry {
                    name: "d1".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: "1".into(),
                },
                NodeEntry {
                    name: "d2".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: "2".into(),
                },
                NodeEntry {
                    name: "my-pattern".into(),
                    kind: NodeKind::Pattern,
                    tags: vec![],
                    hash: "m".into(),
                },
            ],
            edges: vec![
                EdgeEntry {
                    from: "d1".into(),
                    to: "comp".into(),
                    kind: EdgeKind::BelongsTo,
                },
                EdgeEntry {
                    from: "d2".into(),
                    to: "comp".into(),
                    kind: EdgeKind::BelongsTo,
                },
                EdgeEntry {
                    from: "my-pattern".into(),
                    to: "d1".into(),
                    kind: EdgeKind::MemberOf,
                },
                EdgeEntry {
                    from: "my-pattern".into(),
                    to: "d2".into(),
                    kind: EdgeKind::MemberOf,
                },
                EdgeEntry {
                    from: "my-pattern".into(),
                    to: "comp".into(),
                    kind: EdgeKind::AppliesTo,
                },
            ],
        };
        let mut components = BTreeMap::new();
        components.insert(
            "comp".into(),
            ComponentFile {
                component: Component {
                    name: "comp".into(),
                    description: String::new(),
                },
            },
        );
        let mut decisions = BTreeMap::new();
        for n in ["d1", "d2"] {
            decisions.insert(
                n.into(),
                DecisionFile {
                    decision: Decision {
                        component: "comp".into(),
                        choice: n.into(),
                        reason: n.into(),
                        alternatives: vec![],
                        tags: vec![],
                        created: ts(),
                    },
                },
            );
        }
        let mut patterns = BTreeMap::new();
        patterns.insert(
            "my-pattern".into(),
            PatternFile {
                pattern: Pattern {
                    name: "my-pattern".into(),
                    description: "test".into(),
                },
            },
        );

        let g = InMemoryGraph::build(
            &index,
            &arc_map(components),
            &arc_map(decisions),
            &arc_map(patterns),
        );
        let pats = g.patterns_for("comp");
        assert_eq!(pats.len(), 1);
        assert_eq!(pats[0].0.as_ref(), "my-pattern");
    }

    // ── edges_involving ──────────────────────────────────────────────────

    #[test]
    fn edges_involving_returns_both_directions() {
        let g = test_graph();
        let edges = g.edges_involving("auth");
        // Forward: auth → database (ConnectsTo)
        // Reverse: use-jwt → auth (BelongsTo)
        assert_eq!(edges.len(), 2);

        let forward: Vec<_> = edges
            .iter()
            .filter(|(_, _, d)| *d == Direction::Forward)
            .collect();
        assert_eq!(forward.len(), 1);
        assert_eq!(forward[0].0, "database");

        let reverse: Vec<_> = edges
            .iter()
            .filter(|(_, _, d)| *d == Direction::Reverse)
            .collect();
        assert_eq!(reverse.len(), 1);
        assert_eq!(reverse[0].0, "use-jwt");
    }

    #[test]
    fn edges_involving_empty_for_unknown() {
        let g = test_graph();
        assert!(g.edges_involving("nonexistent").is_empty());
    }

    // ── transitive_depends_on ───────────────────────────────────────────

    /// Build a chain graph: d1 → d2 → d3 → d4 (all DependsOn).
    /// All decisions belong to component "chain-comp".
    fn chain_graph() -> InMemoryGraph {
        let index = GraphIndex {
            version: 1,
            rebuilt: ts(),
            nodes: vec![
                NodeEntry {
                    name: "project".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: "p".into(),
                },
                NodeEntry {
                    name: "chain-comp".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: "cc".into(),
                },
                NodeEntry {
                    name: "other-comp".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: "oc".into(),
                },
                NodeEntry {
                    name: "d1".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: "h1".into(),
                },
                NodeEntry {
                    name: "d2".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: "h2".into(),
                },
                NodeEntry {
                    name: "d3".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: "h3".into(),
                },
                NodeEntry {
                    name: "d4".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: "h4".into(),
                },
            ],
            edges: vec![
                EdgeEntry {
                    from: "d1".into(),
                    to: "chain-comp".into(),
                    kind: EdgeKind::BelongsTo,
                },
                EdgeEntry {
                    from: "d2".into(),
                    to: "other-comp".into(),
                    kind: EdgeKind::BelongsTo,
                },
                EdgeEntry {
                    from: "d3".into(),
                    to: "other-comp".into(),
                    kind: EdgeKind::BelongsTo,
                },
                EdgeEntry {
                    from: "d4".into(),
                    to: "other-comp".into(),
                    kind: EdgeKind::BelongsTo,
                },
                EdgeEntry {
                    from: "d1".into(),
                    to: "d2".into(),
                    kind: EdgeKind::DependsOn,
                },
                EdgeEntry {
                    from: "d2".into(),
                    to: "d3".into(),
                    kind: EdgeKind::DependsOn,
                },
                EdgeEntry {
                    from: "d3".into(),
                    to: "d4".into(),
                    kind: EdgeKind::DependsOn,
                },
            ],
        };

        let mut components = BTreeMap::new();
        for name in ["chain-comp", "other-comp"] {
            components.insert(
                name.into(),
                ComponentFile {
                    component: Component {
                        name: name.into(),
                        description: String::new(),
                    },
                },
            );
        }
        let mut decisions = BTreeMap::new();
        for (name, comp) in [
            ("d1", "chain-comp"),
            ("d2", "other-comp"),
            ("d3", "other-comp"),
            ("d4", "other-comp"),
        ] {
            decisions.insert(
                name.into(),
                DecisionFile {
                    decision: Decision {
                        component: comp.into(),
                        choice: format!("choice-{name}"),
                        reason: "test".into(),
                        alternatives: vec![],
                        tags: vec![],
                        created: ts(),
                    },
                },
            );
        }

        InMemoryGraph::build(
            &index,
            &arc_map(components),
            &arc_map(decisions),
            &BTreeMap::new(),
        )
    }

    #[test]
    fn transitive_depth_1() {
        let g = chain_graph();
        let result = g.transitive_depends_on(&["d1"], 1);
        let names: Vec<&str> = result.iter().map(|(n, _)| n.as_ref()).collect();
        assert_eq!(names, vec!["d2"]);
    }

    #[test]
    fn transitive_depth_2() {
        let g = chain_graph();
        let result = g.transitive_depends_on(&["d1"], 2);
        let mut names: Vec<&str> = result.iter().map(|(n, _)| n.as_ref()).collect();
        names.sort_unstable();
        assert_eq!(names, vec!["d2", "d3"]);
    }

    #[test]
    fn transitive_depth_3() {
        let g = chain_graph();
        let result = g.transitive_depends_on(&["d1"], 3);
        let mut names: Vec<&str> = result.iter().map(|(n, _)| n.as_ref()).collect();
        names.sort_unstable();
        assert_eq!(names, vec!["d2", "d3", "d4"]);
    }

    #[test]
    fn transitive_respects_max_depth() {
        let g = chain_graph();
        // d1 → d2 → d3 → d4, but max_depth=2 should stop before d4.
        let result = g.transitive_depends_on(&["d1"], 2);
        let names: Vec<&str> = result.iter().map(|(n, _)| n.as_ref()).collect();
        assert!(!names.contains(&"d4"));
    }

    #[test]
    fn transitive_excludes_seeds() {
        let g = chain_graph();
        let result = g.transitive_depends_on(&["d1"], 3);
        let names: Vec<&str> = result.iter().map(|(n, _)| n.as_ref()).collect();
        assert!(!names.contains(&"d1"), "seed should be excluded");
    }

    #[test]
    fn transitive_multiple_seeds() {
        let g = chain_graph();
        // d1 depends on d2, d2 depends on d3. Seeds are [d1, d2].
        // d2 is excluded (it's a seed). d3 is reachable from d2 at depth 1.
        // d4 is reachable from d3 at depth 2 (from d2's perspective).
        let result = g.transitive_depends_on(&["d1", "d2"], 2);
        let mut names: Vec<&str> = result.iter().map(|(n, _)| n.as_ref()).collect();
        names.sort_unstable();
        assert_eq!(names, vec!["d3", "d4"]);
    }

    #[test]
    fn transitive_empty_for_no_dependencies() {
        let g = chain_graph();
        // d4 has no DependsOn edges.
        let result = g.transitive_depends_on(&["d4"], 3);
        assert!(result.is_empty());
    }

    #[test]
    fn transitive_handles_cycle() {
        // Build a cycle: a → b → a (via DependsOn).
        let index = GraphIndex {
            version: 1,
            rebuilt: ts(),
            nodes: vec![
                NodeEntry {
                    name: "project".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: "p".into(),
                },
                NodeEntry {
                    name: "comp".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: "c".into(),
                },
                NodeEntry {
                    name: "a".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: "ha".into(),
                },
                NodeEntry {
                    name: "b".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: "hb".into(),
                },
            ],
            edges: vec![
                EdgeEntry {
                    from: "a".into(),
                    to: "comp".into(),
                    kind: EdgeKind::BelongsTo,
                },
                EdgeEntry {
                    from: "b".into(),
                    to: "comp".into(),
                    kind: EdgeKind::BelongsTo,
                },
                EdgeEntry {
                    from: "a".into(),
                    to: "b".into(),
                    kind: EdgeKind::DependsOn,
                },
                EdgeEntry {
                    from: "b".into(),
                    to: "a".into(),
                    kind: EdgeKind::DependsOn,
                },
            ],
        };
        let mut components = BTreeMap::new();
        components.insert(
            "comp".into(),
            ComponentFile {
                component: Component {
                    name: "comp".into(),
                    description: String::new(),
                },
            },
        );
        let mut decisions = BTreeMap::new();
        for name in ["a", "b"] {
            decisions.insert(
                name.into(),
                DecisionFile {
                    decision: Decision {
                        component: "comp".into(),
                        choice: name.into(),
                        reason: "test".into(),
                        alternatives: vec![],
                        tags: vec![],
                        created: ts(),
                    },
                },
            );
        }
        let g = InMemoryGraph::build(
            &index,
            &arc_map(components),
            &arc_map(decisions),
            &BTreeMap::new(),
        );

        // Must terminate despite the cycle, and return only b (not a, the seed).
        let result = g.transitive_depends_on(&["a"], 10);
        let names: Vec<&str> = result.iter().map(|(n, _)| n.as_ref()).collect();
        assert_eq!(names, vec!["b"]);
    }

    // ── validate: clean graph ────────────────────────────────────────────

    #[test]
    fn validate_clean_graph() {
        let g = test_graph();
        let issues = g.validate();
        assert!(issues.is_empty(), "expected no issues, got: {issues:?}");
    }

    // ── validate: edge endpoint missing ──────────────────────────────────

    #[test]
    fn validate_catches_dangling_edge_target() {
        let index = GraphIndex {
            version: 1,
            rebuilt: ts(),
            nodes: vec![NodeEntry {
                name: "a".into(),
                kind: NodeKind::Decision,
                tags: vec![],
                hash: "1".into(),
            }],
            edges: vec![EdgeEntry {
                from: "a".into(),
                to: "ghost".into(),
                kind: EdgeKind::BelongsTo,
            }],
        };
        let g = InMemoryGraph::build(&index, &BTreeMap::new(), &BTreeMap::new(), &BTreeMap::new());
        let issues = g.validate();
        assert!(issues.iter().any(|i| i.message.contains("ghost")));
    }

    // ── validate: edge type violations ───────────────────────────────────

    #[test]
    fn validate_catches_belongs_to_wrong_types() {
        let index = GraphIndex {
            version: 1,
            rebuilt: ts(),
            nodes: vec![
                NodeEntry {
                    name: "c1".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: "1".into(),
                },
                NodeEntry {
                    name: "c2".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: "2".into(),
                },
            ],
            edges: vec![
                // BelongsTo must be decision → component, not component → component.
                EdgeEntry {
                    from: "c1".into(),
                    to: "c2".into(),
                    kind: EdgeKind::BelongsTo,
                },
            ],
        };
        let g = InMemoryGraph::build(&index, &BTreeMap::new(), &BTreeMap::new(), &BTreeMap::new());
        let issues = g.validate();
        assert!(
            issues
                .iter()
                .any(|i| i.severity == Severity::Error && i.message.contains("BelongsTo"))
        );
    }

    #[test]
    fn validate_catches_connects_to_wrong_types() {
        let index = GraphIndex {
            version: 1,
            rebuilt: ts(),
            nodes: vec![
                NodeEntry {
                    name: "d1".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: "1".into(),
                },
                NodeEntry {
                    name: "d2".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: "2".into(),
                },
            ],
            edges: vec![EdgeEntry {
                from: "d1".into(),
                to: "d2".into(),
                kind: EdgeKind::ConnectsTo,
            }],
        };
        let g = InMemoryGraph::build(&index, &BTreeMap::new(), &BTreeMap::new(), &BTreeMap::new());
        let issues = g.validate();
        assert!(issues.iter().any(|i| i.message.contains("ConnectsTo")));
    }

    // ── validate: self-edge ──────────────────────────────────────────────

    #[test]
    fn validate_catches_self_edge() {
        let index = GraphIndex {
            version: 1,
            rebuilt: ts(),
            nodes: vec![NodeEntry {
                name: "a".into(),
                kind: NodeKind::Component,
                tags: vec![],
                hash: "1".into(),
            }],
            edges: vec![EdgeEntry {
                from: "a".into(),
                to: "a".into(),
                kind: EdgeKind::ConnectsTo,
            }],
        };
        let g = InMemoryGraph::build(&index, &BTreeMap::new(), &BTreeMap::new(), &BTreeMap::new());
        let issues = g.validate();
        assert!(issues.iter().any(|i| i.message.contains("self-edge")));
    }

    // ── validate: duplicate edge ─────────────────────────────────────────

    #[test]
    fn validate_catches_duplicate_edge() {
        let index = GraphIndex {
            version: 1,
            rebuilt: ts(),
            nodes: vec![
                NodeEntry {
                    name: "a".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: "1".into(),
                },
                NodeEntry {
                    name: "b".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: "2".into(),
                },
            ],
            edges: vec![
                EdgeEntry {
                    from: "a".into(),
                    to: "b".into(),
                    kind: EdgeKind::ConnectsTo,
                },
                EdgeEntry {
                    from: "a".into(),
                    to: "b".into(),
                    kind: EdgeKind::ConnectsTo,
                },
            ],
        };
        let g = InMemoryGraph::build(&index, &BTreeMap::new(), &BTreeMap::new(), &BTreeMap::new());
        let issues = g.validate();
        assert!(issues.iter().any(|i| i.message.contains("duplicate")));
    }

    // ── validate: pattern < 2 members ────────────────────────────────────

    #[test]
    fn validate_catches_pattern_too_few_members() {
        let index = GraphIndex {
            version: 1,
            rebuilt: ts(),
            nodes: vec![
                NodeEntry {
                    name: "pat".into(),
                    kind: NodeKind::Pattern,
                    tags: vec![],
                    hash: "1".into(),
                },
                NodeEntry {
                    name: "d1".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: "2".into(),
                },
            ],
            edges: vec![EdgeEntry {
                from: "pat".into(),
                to: "d1".into(),
                kind: EdgeKind::MemberOf,
            }],
        };
        let g = InMemoryGraph::build(&index, &BTreeMap::new(), &BTreeMap::new(), &BTreeMap::new());
        let issues = g.validate();
        assert!(issues.iter().any(|i| i.message.contains("minimum 2")));
    }

    // ── validate: cycle in depends_on ────────────────────────────────────

    #[test]
    fn validate_catches_depends_on_cycle() {
        let index = GraphIndex {
            version: 1,
            rebuilt: ts(),
            nodes: vec![
                NodeEntry {
                    name: "d1".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: "1".into(),
                },
                NodeEntry {
                    name: "d2".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: "2".into(),
                },
            ],
            edges: vec![
                EdgeEntry {
                    from: "d1".into(),
                    to: "d2".into(),
                    kind: EdgeKind::DependsOn,
                },
                EdgeEntry {
                    from: "d2".into(),
                    to: "d1".into(),
                    kind: EdgeKind::DependsOn,
                },
            ],
        };
        let mut decisions = BTreeMap::new();
        for n in ["d1", "d2"] {
            decisions.insert(
                n.into(),
                DecisionFile {
                    decision: Decision {
                        component: "project".into(),
                        choice: n.into(),
                        reason: n.into(),
                        alternatives: vec![],
                        tags: vec![],
                        created: ts(),
                    },
                },
            );
        }
        let g = InMemoryGraph::build(
            &index,
            &BTreeMap::new(),
            &arc_map(decisions),
            &BTreeMap::new(),
        );
        let issues = g.validate();
        assert!(issues.iter().any(|i| i.message.contains("cycle")));
    }

    // ── validate: empty choice / reason ──────────────────────────────────

    #[test]
    fn validate_catches_empty_choice() {
        let index = GraphIndex {
            version: 1,
            rebuilt: ts(),
            nodes: vec![NodeEntry {
                name: "bad".into(),
                kind: NodeKind::Decision,
                tags: vec![],
                hash: "1".into(),
            }],
            edges: vec![],
        };
        let mut decisions = BTreeMap::new();
        decisions.insert(
            "bad".into(),
            DecisionFile {
                decision: Decision {
                    component: "project".into(),
                    choice: String::new(),
                    reason: "ok".into(),
                    alternatives: vec![],
                    tags: vec![],
                    created: ts(),
                },
            },
        );
        let g = InMemoryGraph::build(
            &index,
            &BTreeMap::new(),
            &arc_map(decisions),
            &BTreeMap::new(),
        );
        let issues = g.validate();
        assert!(issues.iter().any(|i| i.message.contains("empty choice")));
    }

    #[test]
    fn validate_catches_empty_reason() {
        let index = GraphIndex {
            version: 1,
            rebuilt: ts(),
            nodes: vec![NodeEntry {
                name: "bad".into(),
                kind: NodeKind::Decision,
                tags: vec![],
                hash: "1".into(),
            }],
            edges: vec![],
        };
        let mut decisions = BTreeMap::new();
        decisions.insert(
            "bad".into(),
            DecisionFile {
                decision: Decision {
                    component: "project".into(),
                    choice: "ok".into(),
                    reason: "   ".into(),
                    alternatives: vec![],
                    tags: vec![],
                    created: ts(),
                },
            },
        );
        let g = InMemoryGraph::build(
            &index,
            &BTreeMap::new(),
            &arc_map(decisions),
            &BTreeMap::new(),
        );
        let issues = g.validate();
        assert!(issues.iter().any(|i| i.message.contains("empty reason")));
    }

    // ── validate: name integrity ─────────────────────────────────────────

    #[test]
    fn validate_catches_component_name_mismatch() {
        let index = GraphIndex {
            version: 1,
            rebuilt: ts(),
            nodes: vec![NodeEntry {
                name: "auth".into(),
                kind: NodeKind::Component,
                tags: vec![],
                hash: "1".into(),
            }],
            edges: vec![],
        };
        let mut components = BTreeMap::new();
        components.insert(
            "auth".into(),
            ComponentFile {
                component: Component {
                    name: "WRONG".into(),
                    description: String::new(),
                },
            },
        );
        let g = InMemoryGraph::build(
            &index,
            &arc_map(components),
            &BTreeMap::new(),
            &BTreeMap::new(),
        );
        let issues = g.validate();
        assert!(issues.iter().any(|i| i.message.contains("does not match")));
    }

    #[test]
    fn validate_catches_non_kebab_component() {
        let index = GraphIndex {
            version: 1,
            rebuilt: ts(),
            nodes: vec![NodeEntry {
                name: "Bad_Name".into(),
                kind: NodeKind::Component,
                tags: vec![],
                hash: "1".into(),
            }],
            edges: vec![],
        };
        let mut components = BTreeMap::new();
        components.insert(
            "Bad_Name".into(),
            ComponentFile {
                component: Component {
                    name: "Bad_Name".into(),
                    description: String::new(),
                },
            },
        );
        let g = InMemoryGraph::build(
            &index,
            &arc_map(components),
            &BTreeMap::new(),
            &BTreeMap::new(),
        );
        let issues = g.validate();
        assert!(issues.iter().any(|i| i.message.contains("kebab-case")));
    }

    // ── to_index ─────────────────────────────────────────────────────────

    #[test]
    fn to_index_sorted_deterministic() {
        let g = test_graph();
        let idx = g.to_index();

        // Nodes sorted by name.
        let names: Vec<&str> = idx.nodes.iter().map(|n| n.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);

        // Edges sorted by (from, to, kind).
        for w in idx.edges.windows(2) {
            assert!(
                (&w[0].from, &w[0].to, &w[0].kind) <= (&w[1].from, &w[1].to, &w[1].kind),
                "edges not sorted: ({}, {}) before ({}, {})",
                w[0].from,
                w[0].to,
                w[1].from,
                w[1].to,
            );
        }
    }

    #[test]
    fn to_index_round_trips() {
        let g = test_graph();
        let idx = g.to_index();

        // Rebuild from the exported index.
        let mut components = BTreeMap::new();
        for (k, v) in &g.components {
            components.insert(k.to_string(), v.clone());
        }
        let mut decisions = BTreeMap::new();
        for (k, v) in &g.decisions {
            decisions.insert(k.to_string(), v.clone());
        }
        let g2 = InMemoryGraph::build(&idx, &components, &decisions, &BTreeMap::new());

        assert_eq!(g2.component_count(), g.component_count());
        assert_eq!(g2.decision_count(), g.decision_count());

        let idx2 = g2.to_index();
        assert_eq!(idx.nodes.len(), idx2.nodes.len());
        assert_eq!(idx.edges.len(), idx2.edges.len());
        for (a, b) in idx.nodes.iter().zip(idx2.nodes.iter()) {
            assert_eq!(a.name, b.name);
            assert_eq!(a.kind, b.kind);
        }
        for (a, b) in idx.edges.iter().zip(idx2.edges.iter()) {
            assert_eq!(a.from, b.from);
            assert_eq!(a.to, b.to);
            assert_eq!(a.kind, b.kind);
        }
    }

    // ── validate: BelongsTo integrity ────────────────────────────────────

    #[test]
    fn validate_catches_missing_belongs_to() {
        let index = GraphIndex {
            version: 1,
            rebuilt: ts(),
            nodes: vec![
                NodeEntry {
                    name: "comp".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: "1".into(),
                },
                NodeEntry {
                    name: "dec".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: "2".into(),
                },
            ],
            edges: vec![], // no BelongsTo edge
        };
        let mut decisions = BTreeMap::new();
        decisions.insert(
            "dec".into(),
            DecisionFile {
                decision: Decision {
                    component: "comp".into(),
                    choice: "test".into(),
                    reason: "test".into(),
                    alternatives: vec![],
                    tags: vec![],
                    created: ts(),
                },
            },
        );
        let mut components = BTreeMap::new();
        components.insert(
            "comp".into(),
            ComponentFile {
                component: Component {
                    name: "comp".into(),
                    description: String::new(),
                },
            },
        );
        let g = InMemoryGraph::build(
            &index,
            &arc_map(components),
            &arc_map(decisions),
            &BTreeMap::new(),
        );
        let issues = g.validate();
        assert!(issues.iter().any(|i| i.message.contains("no BelongsTo")));
    }

    #[test]
    fn validate_catches_duplicate_belongs_to() {
        let index = GraphIndex {
            version: 1,
            rebuilt: ts(),
            nodes: vec![
                NodeEntry {
                    name: "comp-a".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: "1".into(),
                },
                NodeEntry {
                    name: "comp-b".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: "2".into(),
                },
                NodeEntry {
                    name: "dec".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: "3".into(),
                },
            ],
            edges: vec![
                EdgeEntry {
                    from: "dec".into(),
                    to: "comp-a".into(),
                    kind: EdgeKind::BelongsTo,
                },
                EdgeEntry {
                    from: "dec".into(),
                    to: "comp-b".into(),
                    kind: EdgeKind::BelongsTo,
                },
            ],
        };
        let mut decisions = BTreeMap::new();
        decisions.insert(
            "dec".into(),
            DecisionFile {
                decision: Decision {
                    component: "comp-a".into(),
                    choice: "test".into(),
                    reason: "test".into(),
                    alternatives: vec![],
                    tags: vec![],
                    created: ts(),
                },
            },
        );
        let mut components = BTreeMap::new();
        components.insert(
            "comp-a".into(),
            ComponentFile {
                component: Component {
                    name: "comp-a".into(),
                    description: String::new(),
                },
            },
        );
        components.insert(
            "comp-b".into(),
            ComponentFile {
                component: Component {
                    name: "comp-b".into(),
                    description: String::new(),
                },
            },
        );
        let g = InMemoryGraph::build(
            &index,
            &arc_map(components),
            &arc_map(decisions),
            &BTreeMap::new(),
        );
        let issues = g.validate();
        assert!(
            issues
                .iter()
                .any(|i| i.message.contains("2 BelongsTo edges"))
        );
    }

    #[test]
    fn validate_catches_belongs_to_target_mismatch() {
        let index = GraphIndex {
            version: 1,
            rebuilt: ts(),
            nodes: vec![
                NodeEntry {
                    name: "comp-a".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: "1".into(),
                },
                NodeEntry {
                    name: "comp-b".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: "2".into(),
                },
                NodeEntry {
                    name: "dec".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: "3".into(),
                },
            ],
            edges: vec![EdgeEntry {
                from: "dec".into(),
                to: "comp-a".into(),
                kind: EdgeKind::BelongsTo,
            }],
        };
        let mut decisions = BTreeMap::new();
        decisions.insert(
            "dec".into(),
            DecisionFile {
                decision: Decision {
                    component: "comp-b".into(), // does NOT match the edge target
                    choice: "test".into(),
                    reason: "test".into(),
                    alternatives: vec![],
                    tags: vec![],
                    created: ts(),
                },
            },
        );
        let mut components = BTreeMap::new();
        components.insert(
            "comp-a".into(),
            ComponentFile {
                component: Component {
                    name: "comp-a".into(),
                    description: String::new(),
                },
            },
        );
        components.insert(
            "comp-b".into(),
            ComponentFile {
                component: Component {
                    name: "comp-b".into(),
                    description: String::new(),
                },
            },
        );
        let g = InMemoryGraph::build(
            &index,
            &arc_map(components),
            &arc_map(decisions),
            &BTreeMap::new(),
        );
        let issues = g.validate();
        assert!(
            issues
                .iter()
                .any(|i| i.message.contains("comp-a") && i.message.contains("comp-b"))
        );
    }

    // ── validate: pattern content ────────────────────────────────────────

    #[test]
    fn validate_catches_empty_pattern_name() {
        let index = GraphIndex {
            version: 1,
            rebuilt: ts(),
            nodes: vec![NodeEntry {
                name: "pat".into(),
                kind: NodeKind::Pattern,
                tags: vec![],
                hash: "1".into(),
            }],
            edges: vec![],
        };
        let mut patterns = BTreeMap::new();
        patterns.insert(
            "pat".into(),
            PatternFile {
                pattern: Pattern {
                    name: String::new(),
                    description: "something".into(),
                },
            },
        );
        let g = InMemoryGraph::build(
            &index,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &arc_map(patterns),
        );
        let issues = g.validate();
        assert!(issues.iter().any(|i| i.message.contains("empty name")));
    }

    #[test]
    fn validate_allows_pattern_name_different_from_key() {
        let index = GraphIndex {
            version: 1,
            rebuilt: ts(),
            nodes: vec![
                NodeEntry {
                    name: "project".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: "p".into(),
                },
                NodeEntry {
                    name: "d1".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: "1".into(),
                },
                NodeEntry {
                    name: "d2".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: "2".into(),
                },
                NodeEntry {
                    name: "state-in-redis".into(),
                    kind: NodeKind::Pattern,
                    tags: vec![],
                    hash: "3".into(),
                },
            ],
            edges: vec![
                EdgeEntry {
                    from: "d1".into(),
                    to: "project".into(),
                    kind: EdgeKind::BelongsTo,
                },
                EdgeEntry {
                    from: "d2".into(),
                    to: "project".into(),
                    kind: EdgeKind::BelongsTo,
                },
                EdgeEntry {
                    from: "state-in-redis".into(),
                    to: "d1".into(),
                    kind: EdgeKind::MemberOf,
                },
                EdgeEntry {
                    from: "state-in-redis".into(),
                    to: "d2".into(),
                    kind: EdgeKind::MemberOf,
                },
            ],
        };
        let mut decisions = BTreeMap::new();
        for n in ["d1", "d2"] {
            decisions.insert(
                n.into(),
                DecisionFile {
                    decision: Decision {
                        component: "project".into(),
                        choice: n.into(),
                        reason: n.into(),
                        alternatives: vec![],
                        tags: vec![],
                        created: ts(),
                    },
                },
            );
        }
        let mut patterns = BTreeMap::new();
        patterns.insert(
            "state-in-redis".into(),
            PatternFile {
                pattern: Pattern {
                    // Human-readable name, intentionally different from the key.
                    name: "All persistent state uses Redis".into(),
                    description: "Shared Redis pool via app state".into(),
                },
            },
        );
        let g = InMemoryGraph::build(
            &index,
            &BTreeMap::new(),
            &arc_map(decisions),
            &arc_map(patterns),
        );
        let issues = g.validate();
        // Must NOT produce any warnings or errors about name mismatch.
        assert!(
            issues.is_empty(),
            "expected no issues, got: {:?}",
            issues.iter().map(|i| &i.message).collect::<Vec<_>>()
        );
    }

    // ── validate: node-content coherence ─────────────────────────────────

    #[test]
    fn validate_catches_orphan_node_without_content() {
        let index = GraphIndex {
            version: 1,
            rebuilt: ts(),
            nodes: vec![
                NodeEntry {
                    name: "project".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: "p".into(),
                },
                NodeEntry {
                    name: "ghost".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: "g".into(),
                },
            ],
            edges: vec![],
        };
        // ghost exists in nodes but has no DecisionFile in the content map.
        let g = InMemoryGraph::build(&index, &BTreeMap::new(), &BTreeMap::new(), &BTreeMap::new());
        let issues = g.validate();
        assert!(
            issues
                .iter()
                .any(|i| i.message.contains("ghost") && i.message.contains("no content")),
            "should flag orphan node: {:?}",
            issues.iter().map(|i| &i.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn validate_allows_project_virtual_node_without_content() {
        let index = GraphIndex {
            version: 1,
            rebuilt: ts(),
            nodes: vec![NodeEntry {
                name: "project".into(),
                kind: NodeKind::Component,
                tags: vec![],
                hash: "p".into(),
            }],
            edges: vec![],
        };
        // "project" has no ComponentFile — it's virtual. Must not error.
        let g = InMemoryGraph::build(&index, &BTreeMap::new(), &BTreeMap::new(), &BTreeMap::new());
        let issues = g.validate();
        assert!(
            !issues.iter().any(|i| i.message.contains("no content")),
            "project virtual node should be exempt: {:?}",
            issues.iter().map(|i| &i.message).collect::<Vec<_>>()
        );
    }

    // ── validate: decision key kebab-case ────────────────────────────────

    #[test]
    fn validate_catches_non_kebab_decision_key() {
        let index = GraphIndex {
            version: 1,
            rebuilt: ts(),
            nodes: vec![
                NodeEntry {
                    name: "project".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: "p".into(),
                },
                NodeEntry {
                    name: "Bad_Key".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: "b".into(),
                },
            ],
            edges: vec![EdgeEntry {
                from: "Bad_Key".into(),
                to: "project".into(),
                kind: EdgeKind::BelongsTo,
            }],
        };
        let mut decisions = BTreeMap::new();
        decisions.insert(
            "Bad_Key".into(),
            DecisionFile {
                decision: Decision {
                    component: "project".into(),
                    choice: "test".into(),
                    reason: "test".into(),
                    alternatives: vec![],
                    tags: vec![],
                    created: ts(),
                },
            },
        );
        let g = InMemoryGraph::build(
            &index,
            &BTreeMap::new(),
            &arc_map(decisions),
            &BTreeMap::new(),
        );
        let issues = g.validate();
        assert!(
            issues
                .iter()
                .any(|i| i.message.contains("Bad_Key") && i.message.contains("kebab-case")),
            "should flag non-kebab decision key: {:?}",
            issues.iter().map(|i| &i.message).collect::<Vec<_>>()
        );
    }

    // ── cascade checks ──────────────────────────────────────────────────

    fn cascade_graph() -> InMemoryGraph {
        let index = GraphIndex {
            version: 1,
            rebuilt: ts(),
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
                    tags: vec![],
                    hash: "a".into(),
                },
                NodeEntry {
                    name: "database".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: "d".into(),
                },
                NodeEntry {
                    name: "use-jwt".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: "1".into(),
                },
                NodeEntry {
                    name: "token-expiry".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: "2".into(),
                },
                NodeEntry {
                    name: "db-pool".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: "3".into(),
                },
                NodeEntry {
                    name: "auth-pattern".into(),
                    kind: NodeKind::Pattern,
                    tags: vec![],
                    hash: "4".into(),
                },
            ],
            edges: vec![
                EdgeEntry {
                    from: "use-jwt".into(),
                    to: "auth".into(),
                    kind: EdgeKind::BelongsTo,
                },
                EdgeEntry {
                    from: "token-expiry".into(),
                    to: "auth".into(),
                    kind: EdgeKind::BelongsTo,
                },
                EdgeEntry {
                    from: "db-pool".into(),
                    to: "database".into(),
                    kind: EdgeKind::BelongsTo,
                },
                EdgeEntry {
                    from: "token-expiry".into(),
                    to: "use-jwt".into(),
                    kind: EdgeKind::DependsOn,
                },
                EdgeEntry {
                    from: "auth".into(),
                    to: "database".into(),
                    kind: EdgeKind::ConnectsTo,
                },
                EdgeEntry {
                    from: "auth-pattern".into(),
                    to: "use-jwt".into(),
                    kind: EdgeKind::MemberOf,
                },
                EdgeEntry {
                    from: "auth-pattern".into(),
                    to: "token-expiry".into(),
                    kind: EdgeKind::MemberOf,
                },
                EdgeEntry {
                    from: "auth-pattern".into(),
                    to: "auth".into(),
                    kind: EdgeKind::AppliesTo,
                },
            ],
        };

        let mut components = BTreeMap::new();
        for name in ["auth", "database"] {
            components.insert(
                name.into(),
                ComponentFile {
                    component: Component {
                        name: name.into(),
                        description: String::new(),
                    },
                },
            );
        }
        let mut decisions = BTreeMap::new();
        for (name, comp) in [
            ("use-jwt", "auth"),
            ("token-expiry", "auth"),
            ("db-pool", "database"),
        ] {
            decisions.insert(
                name.into(),
                DecisionFile {
                    decision: Decision {
                        component: comp.into(),
                        choice: name.into(),
                        reason: "test".into(),
                        alternatives: vec![],
                        tags: vec![],
                        created: ts(),
                    },
                },
            );
        }
        let mut patterns = BTreeMap::new();
        patterns.insert(
            "auth-pattern".into(),
            PatternFile {
                pattern: Pattern {
                    name: "Auth pattern".into(),
                    description: "test".into(),
                },
            },
        );

        InMemoryGraph::build(
            &index,
            &arc_map(components),
            &arc_map(decisions),
            &arc_map(patterns),
        )
    }

    #[test]
    fn cascade_decision_blocks_when_depended_on() {
        let g = cascade_graph();
        let r = g.check_decision_cascade("use-jwt");
        assert!(r.is_blocked());
        assert!(
            r.blockers
                .iter()
                .any(|b| b.node == "token-expiry" && b.edge == EdgeKind::DependsOn)
        );
    }

    #[test]
    fn cascade_decision_blocks_when_pattern_would_shrink() {
        let g = cascade_graph();
        // token-expiry is a member of auth-pattern (2 members total).
        let r = g.check_decision_cascade("token-expiry");
        // Also blocked by nothing depending on it via DependsOn (only use-jwt has
        // DependsOn, pointing the other way). But pattern check should block.
        assert!(r.is_blocked());
        assert!(r.blockers.iter().any(|b| b.node == "auth-pattern"
            && b.edge == EdgeKind::MemberOf
            && b.message.contains("fewer than 2")));
    }

    #[test]
    fn cascade_decision_allows_independent() {
        let g = cascade_graph();
        let r = g.check_decision_cascade("db-pool");
        assert!(!r.is_blocked());
    }

    #[test]
    fn cascade_decision_collects_cleanups() {
        let g = cascade_graph();
        let r = g.check_decision_cascade("db-pool");
        // db-pool has a forward BelongsTo edge to database.
        assert!(
            r.cleanups
                .iter()
                .any(|c| c.edge == EdgeKind::BelongsTo && c.target == "database")
        );
    }

    #[test]
    fn cascade_component_blocks_when_has_decisions() {
        let g = cascade_graph();
        let r = g.check_component_cascade("auth");
        assert!(r.is_blocked());
        assert!(
            r.blockers
                .iter()
                .any(|b| b.node == "use-jwt" || b.node == "token-expiry")
        );
    }

    #[test]
    fn cascade_component_warns_about_patterns_and_connections() {
        let g = cascade_graph();
        let r = g.check_component_cascade("database");
        // database has no decisions belonging to it except db-pool, but
        // db-pool's BelongsTo edge makes it blocked.
        assert!(r.is_blocked());
        // Also has incoming ConnectsTo from auth.
        // Rebuild without the decision to test warnings only.
        let index = GraphIndex {
            version: 1,
            rebuilt: ts(),
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
                    tags: vec![],
                    hash: "a".into(),
                },
                NodeEntry {
                    name: "database".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: "d".into(),
                },
            ],
            edges: vec![EdgeEntry {
                from: "auth".into(),
                to: "database".into(),
                kind: EdgeKind::ConnectsTo,
            }],
        };
        let mut components = BTreeMap::new();
        for name in ["auth", "database"] {
            components.insert(
                name.into(),
                ComponentFile {
                    component: Component {
                        name: name.into(),
                        description: String::new(),
                    },
                },
            );
        }
        let g2 = InMemoryGraph::build(
            &index,
            &arc_map(components),
            &BTreeMap::new(),
            &BTreeMap::new(),
        );
        let r2 = g2.check_component_cascade("database");
        assert!(!r2.is_blocked());
        assert!(
            r2.warnings
                .iter()
                .any(|w| w.node == "auth" && w.edge == EdgeKind::ConnectsTo)
        );
    }

    #[test]
    fn cascade_component_allows_empty() {
        let index = GraphIndex {
            version: 1,
            rebuilt: ts(),
            nodes: vec![
                NodeEntry {
                    name: "project".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: "p".into(),
                },
                NodeEntry {
                    name: "orphan".into(),
                    kind: NodeKind::Component,
                    tags: vec![],
                    hash: "o".into(),
                },
            ],
            edges: vec![],
        };
        let mut components = BTreeMap::new();
        components.insert(
            "orphan".into(),
            ComponentFile {
                component: Component {
                    name: "orphan".into(),
                    description: String::new(),
                },
            },
        );
        let g = InMemoryGraph::build(
            &index,
            &arc_map(components),
            &BTreeMap::new(),
            &BTreeMap::new(),
        );
        let r = g.check_component_cascade("orphan");
        assert!(!r.is_blocked());
        assert!(r.warnings.is_empty());
        assert!(r.cleanups.is_empty());
    }

    #[test]
    fn cascade_result_is_blocked_semantics() {
        let empty = CascadeResult {
            blockers: vec![],
            warnings: vec![CascadeEffect {
                node: "x".into(),
                edge: EdgeKind::Constrains,
                message: "something".into(),
            }],
            cleanups: vec![],
        };
        assert!(!empty.is_blocked());
        let blocked = CascadeResult {
            blockers: vec![CascadeBlocker {
                node: "y".into(),
                edge: EdgeKind::DependsOn,
                message: "reason".into(),
            }],
            warnings: vec![],
            cleanups: vec![],
        };
        assert!(blocked.is_blocked());
    }

    #[test]
    fn cascade_blocker_summary_joins_messages() {
        let result = CascadeResult {
            blockers: vec![
                CascadeBlocker {
                    node: "a".into(),
                    edge: EdgeKind::DependsOn,
                    message: "first".into(),
                },
                CascadeBlocker {
                    node: "b".into(),
                    edge: EdgeKind::MemberOf,
                    message: "second".into(),
                },
            ],
            warnings: vec![],
            cleanups: vec![],
        };
        assert_eq!(result.blocker_summary(), "first; second");
    }
}
