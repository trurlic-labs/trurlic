use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use chrono::Utc;

use super::schema::{
    ComponentFile, DecisionFile, EdgeEntry, EdgeKind, GraphIndex, NodeEntry, NodeKind, PatternFile,
};
use super::state::is_valid_kebab_case;

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

// ── InMemoryGraph ───────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct InMemoryGraph {
    nodes: HashMap<Arc<str>, NodeMeta>,
    forward: HashMap<Arc<str>, Vec<Edge>>,
    reverse: HashMap<Arc<str>, Vec<Edge>>,
    components: HashMap<Arc<str>, ComponentFile>,
    decisions: HashMap<Arc<str>, DecisionFile>,
    patterns: HashMap<Arc<str>, PatternFile>,
}

impl InMemoryGraph {
    /// Build from a parsed [`GraphIndex`] and content maps.
    ///
    /// All node names are interned as [`Arc<str>`] for zero-cost sharing
    /// across adjacency maps, content caches, and query results.
    pub fn build(
        index: &GraphIndex,
        components: HashMap<String, ComponentFile>,
        decisions: HashMap<String, DecisionFile>,
        patterns: HashMap<String, PatternFile>,
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
                .into_iter()
                .map(|(k, v)| (intern(&k), v))
                .collect(),
            decisions: decisions
                .into_iter()
                .map(|(k, v)| (intern(&k), v))
                .collect(),
            patterns: patterns.into_iter().map(|(k, v)| (intern(&k), v)).collect(),
        }
    }

    // ── Queries ──────────────────────────────────────────────────────────

    /// All decisions belonging to a component (reverse `BelongsTo` edges).
    pub fn decisions_for(&self, component: &str) -> Vec<(&Arc<str>, &DecisionFile)> {
        self.reverse_targets(component, EdgeKind::BelongsTo, |name| {
            self.decisions.get(name).map(|d| (name, d))
        })
    }

    /// All project-wide decisions (`BelongsTo "project"`).
    pub fn project_decisions(&self) -> Vec<(&Arc<str>, &DecisionFile)> {
        self.decisions_for("project")
    }

    /// Components this component connects to (forward `ConnectsTo`).
    pub fn connects_to(&self, component: &str) -> Vec<&Arc<str>> {
        self.forward_targets(component, EdgeKind::ConnectsTo)
    }

    /// Components that connect to this one (reverse `ConnectsTo`).
    pub fn connects_from(&self, component: &str) -> Vec<&Arc<str>> {
        self.reverse_targets(component, EdgeKind::ConnectsTo, Some)
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

    /// Transitive dependencies via `DependsOn` edges. BFS, max depth 3.
    pub fn transitive_deps(&self, decision: &str) -> Vec<&Arc<str>> {
        let mut result = Vec::new();
        let mut visited: HashSet<&str> = HashSet::new();
        visited.insert(decision);

        let mut current: Vec<&str> = vec![decision];
        for _depth in 0..3 {
            let mut next = Vec::new();
            for node in &current {
                if let Some(edges) = self.forward.get(*node) {
                    for edge in edges {
                        if edge.kind == EdgeKind::DependsOn && visited.insert(&edge.target) {
                            next.push(edge.target.as_ref());
                            result.push(&edge.target);
                        }
                    }
                }
            }
            if next.is_empty() {
                break;
            }
            current = next;
        }
        result
    }

    /// Patterns that apply to a component (reverse `AppliesTo` edges).
    pub fn patterns_for(&self, component: &str) -> Vec<(&Arc<str>, &PatternFile)> {
        self.reverse_targets(component, EdgeKind::AppliesTo, |name| {
            self.patterns.get(name).map(|p| (name, p))
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
            self.patterns.get(name).map(|p| (name, p))
        })
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
    pub fn edges_involving(&self, node: &str) -> Vec<(Arc<str>, &Edge, Direction)> {
        let mut result = Vec::new();
        if let Some(edges) = self.forward.get(node) {
            for edge in edges {
                result.push((edge.target.clone(), edge, Direction::Forward));
            }
        }
        if let Some(edges) = self.reverse.get(node) {
            for edge in edges {
                result.push((edge.target.clone(), edge, Direction::Reverse));
            }
        }
        result
    }

    /// Check if adding a `DependsOn` edge from → to would create a cycle.
    ///
    /// Returns `true` if `from` is reachable from `to` via existing
    /// `DependsOn` edges (meaning the new edge would close a loop).
    pub fn would_cycle(&self, from: &str, to: &str) -> bool {
        if from == to {
            return true;
        }
        // BFS from `to`: if we can reach `from`, the new edge creates a cycle.
        let mut visited: HashSet<&str> = HashSet::new();
        let mut queue: VecDeque<&str> = VecDeque::new();
        queue.push_back(to);
        visited.insert(to);

        while let Some(current) = queue.pop_front() {
            if let Some(edges) = self.forward.get(current) {
                for edge in edges {
                    if edge.kind == EdgeKind::DependsOn {
                        if edge.target.as_ref() == from {
                            return true;
                        }
                        if visited.insert(&edge.target) {
                            queue.push_back(&edge.target);
                        }
                    }
                }
            }
        }
        false
    }

    // ── Content access ───────────────────────────────────────────────────

    pub fn component(&self, name: &str) -> Option<&ComponentFile> {
        self.components.get(name)
    }

    pub fn decision(&self, name: &str) -> Option<&DecisionFile> {
        self.decisions.get(name)
    }

    pub fn pattern(&self, name: &str) -> Option<&PatternFile> {
        self.patterns.get(name)
    }

    pub fn node_meta(&self, name: &str) -> Option<&NodeMeta> {
        self.nodes.get(name)
    }

    pub fn component_count(&self) -> usize {
        self.components.len()
    }

    pub fn decision_count(&self) -> usize {
        self.decisions.len()
    }

    pub fn pattern_count(&self) -> usize {
        self.patterns.len()
    }

    // ── Validation ───────────────────────────────────────────────────────

    /// Full graph integrity check. Returns empty vec when valid.
    pub fn validate(&self) -> Vec<Issue> {
        let mut issues = Vec::new();
        self.check_edge_endpoints(&mut issues);
        self.check_edge_type_constraints(&mut issues);
        self.check_self_edges(&mut issues);
        self.check_duplicate_edges(&mut issues);
        self.check_pattern_membership(&mut issues);
        self.check_depends_on_cycles(&mut issues);
        self.check_belongs_to_integrity(&mut issues);
        self.check_content_integrity(&mut issues);
        self.check_name_integrity(&mut issues);
        issues
    }

    // ── Serialization ────────────────────────────────────────────────────

    /// Export current state as [`GraphIndex`] (sorted for deterministic output).
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
    /// Collect forward edge targets of a specific kind.
    fn forward_targets(&self, node: &str, kind: EdgeKind) -> Vec<&Arc<str>> {
        self.forward
            .get(node)
            .map(|edges| {
                edges
                    .iter()
                    .filter(|e| e.kind == kind)
                    .map(|e| &e.target)
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

    // ── Validation checks ────────────────────────────────────────────────

    /// Checks 1-2: every edge endpoint exists in nodes.
    fn check_edge_endpoints(&self, issues: &mut Vec<Issue>) {
        for (from, edge_list) in &self.forward {
            if !self.nodes.contains_key(from) {
                issues.push(Issue {
                    severity: Severity::Error,
                    message: format!("edge source `{from}` is not a known node"),
                    node: Some(from.to_string()),
                });
            }
            for edge in edge_list {
                if !self.nodes.contains_key(&edge.target) {
                    issues.push(Issue {
                        severity: Severity::Error,
                        message: format!(
                            "edge target `{}` (from `{from}`, {:?}) is not a known node",
                            edge.target, edge.kind
                        ),
                        node: Some(edge.target.to_string()),
                    });
                }
            }
        }
    }

    /// Checks 3-7: edge kind → node kind constraints.
    fn check_edge_type_constraints(&self, issues: &mut Vec<Issue>) {
        for (from, edge_list) in &self.forward {
            let from_kind = self.nodes.get(from).map(|m| m.kind);
            for edge in edge_list {
                let to_kind = self.nodes.get(&edge.target).map(|m| m.kind);
                let (from_k, to_k) = match (from_kind, to_kind) {
                    (Some(f), Some(t)) => (f, t),
                    _ => continue, // endpoint-missing already reported
                };

                let violation = match edge.kind {
                    EdgeKind::BelongsTo => {
                        from_k != NodeKind::Decision || to_k != NodeKind::Component
                    }
                    EdgeKind::ConnectsTo => {
                        from_k != NodeKind::Component || to_k != NodeKind::Component
                    }
                    EdgeKind::DependsOn | EdgeKind::Constrains | EdgeKind::Supersedes => {
                        from_k != NodeKind::Decision || to_k != NodeKind::Decision
                    }
                    EdgeKind::MemberOf => from_k != NodeKind::Pattern || to_k != NodeKind::Decision,
                    EdgeKind::AppliesTo => {
                        from_k != NodeKind::Pattern || to_k != NodeKind::Component
                    }
                };

                if violation {
                    issues.push(Issue {
                        severity: Severity::Error,
                        message: format!(
                            "{:?} edge `{from}` ({from_k:?}) → `{}` ({to_k:?}): \
                             invalid node kinds",
                            edge.kind, edge.target
                        ),
                        node: Some(from.to_string()),
                    });
                }
            }
        }
    }

    /// Check 14: no self-edges.
    fn check_self_edges(&self, issues: &mut Vec<Issue>) {
        for (from, edge_list) in &self.forward {
            for edge in edge_list {
                if *from == edge.target {
                    issues.push(Issue {
                        severity: Severity::Error,
                        message: format!("self-edge on `{from}` ({:?})", edge.kind),
                        node: Some(from.to_string()),
                    });
                }
            }
        }
    }

    /// Check 10: no duplicate edges (same from + to + kind).
    fn check_duplicate_edges(&self, issues: &mut Vec<Issue>) {
        let mut seen: HashSet<(&str, &str, EdgeKind)> = HashSet::new();
        for (from, edge_list) in &self.forward {
            for edge in edge_list {
                if !seen.insert((from, &edge.target, edge.kind)) {
                    issues.push(Issue {
                        severity: Severity::Error,
                        message: format!(
                            "duplicate {:?} edge `{from}` → `{}`",
                            edge.kind, edge.target
                        ),
                        node: Some(from.to_string()),
                    });
                }
            }
        }
    }

    /// Check 8: every pattern has ≥ 2 MemberOf edges.
    fn check_pattern_membership(&self, issues: &mut Vec<Issue>) {
        for (name, meta) in &self.nodes {
            if meta.kind != NodeKind::Pattern {
                continue;
            }
            let member_count = self
                .forward
                .get(name)
                .map(|edges| {
                    edges
                        .iter()
                        .filter(|e| e.kind == EdgeKind::MemberOf)
                        .count()
                })
                .unwrap_or(0);
            if member_count < 2 {
                issues.push(Issue {
                    severity: Severity::Error,
                    message: format!(
                        "pattern `{name}` has {member_count} member decision(s) (minimum 2)"
                    ),
                    node: Some(name.to_string()),
                });
            }
        }
    }

    /// Check 9: no cycles in the DependsOn subgraph.
    fn check_depends_on_cycles(&self, issues: &mut Vec<Issue>) {
        let mut visited: HashSet<&str> = HashSet::new();
        let mut in_stack: HashSet<&str> = HashSet::new();

        for (name, meta) in &self.nodes {
            if meta.kind == NodeKind::Decision
                && !visited.contains(name.as_ref())
                && self.dfs_has_cycle(name, &mut visited, &mut in_stack)
            {
                issues.push(Issue {
                    severity: Severity::Error,
                    message: format!("cycle in depends_on chain involving `{name}`"),
                    node: Some(name.to_string()),
                });
            }
        }
    }

    fn dfs_has_cycle<'a>(
        &'a self,
        node: &'a str,
        visited: &mut HashSet<&'a str>,
        in_stack: &mut HashSet<&'a str>,
    ) -> bool {
        visited.insert(node);
        in_stack.insert(node);

        if let Some(edges) = self.forward.get(node) {
            for edge in edges {
                if edge.kind != EdgeKind::DependsOn {
                    continue;
                }
                let target: &str = &edge.target;
                if in_stack.contains(target) {
                    return true;
                }
                if !visited.contains(target) && self.dfs_has_cycle(target, visited, in_stack) {
                    return true;
                }
            }
        }

        in_stack.remove(node);
        false
    }

    /// Checks 11-13: content integrity.
    fn check_content_integrity(&self, issues: &mut Vec<Issue>) {
        for (name, dec) in &self.decisions {
            if dec.decision.choice.trim().is_empty() {
                issues.push(Issue {
                    severity: Severity::Error,
                    message: format!("decision `{name}` has empty choice"),
                    node: Some(name.to_string()),
                });
            }
            if dec.decision.reason.trim().is_empty() {
                issues.push(Issue {
                    severity: Severity::Error,
                    message: format!("decision `{name}` has empty reason"),
                    node: Some(name.to_string()),
                });
            }
            let comp = &dec.decision.component;
            if comp != "project" && !self.components.contains_key(comp.as_str()) {
                issues.push(Issue {
                    severity: Severity::Error,
                    message: format!(
                        "decision `{name}` references component `{comp}` which does not exist"
                    ),
                    node: Some(name.to_string()),
                });
            }
            if comp != "project" && !is_valid_kebab_case(comp) {
                issues.push(Issue {
                    severity: Severity::Error,
                    message: format!(
                        "decision `{name}` has invalid component `{comp}` \
                         (must be kebab-case or \"project\")"
                    ),
                    node: Some(name.to_string()),
                });
            }
        }
        for (name, pat) in &self.patterns {
            if pat.pattern.name.trim().is_empty() {
                issues.push(Issue {
                    severity: Severity::Error,
                    message: format!("pattern `{name}` has empty name"),
                    node: Some(name.to_string()),
                });
            }
            if pat.pattern.description.trim().is_empty() {
                issues.push(Issue {
                    severity: Severity::Warning,
                    message: format!("pattern `{name}` has empty description"),
                    node: Some(name.to_string()),
                });
            }
        }
    }

    /// Every Decision node has exactly one BelongsTo edge whose target matches
    /// the `decision.component` field in the node file.
    fn check_belongs_to_integrity(&self, issues: &mut Vec<Issue>) {
        for (name, meta) in &self.nodes {
            if meta.kind != NodeKind::Decision {
                continue;
            }
            let belongs_to: Vec<&Edge> = self
                .forward
                .get(name)
                .map(|edges| {
                    edges
                        .iter()
                        .filter(|e| e.kind == EdgeKind::BelongsTo)
                        .collect()
                })
                .unwrap_or_default();

            if belongs_to.is_empty() {
                issues.push(Issue {
                    severity: Severity::Error,
                    message: format!("decision `{name}` has no BelongsTo edge"),
                    node: Some(name.to_string()),
                });
            } else if belongs_to.len() > 1 {
                issues.push(Issue {
                    severity: Severity::Error,
                    message: format!(
                        "decision `{name}` has {} BelongsTo edges (must be exactly 1)",
                        belongs_to.len()
                    ),
                    node: Some(name.to_string()),
                });
            }

            // Verify BelongsTo target matches the decision file's component field.
            if let Some(dec) = self.decisions.get(name) {
                for edge in &belongs_to {
                    if edge.target.as_ref() != dec.decision.component {
                        issues.push(Issue {
                            severity: Severity::Error,
                            message: format!(
                                "decision `{name}` BelongsTo target `{}` does not match \
                                 decision.component `{}`",
                                edge.target, dec.decision.component
                            ),
                            node: Some(name.to_string()),
                        });
                    }
                }
            }
        }
    }

    fn check_name_integrity(&self, issues: &mut Vec<Issue>) {
        for (key, comp) in &self.components {
            if key.as_ref() != comp.component.name {
                issues.push(Issue {
                    severity: Severity::Error,
                    message: format!(
                        "component key `{key}` does not match internal name `{}`",
                        comp.component.name
                    ),
                    node: Some(key.to_string()),
                });
            }
            if !is_valid_kebab_case(&comp.component.name) {
                issues.push(Issue {
                    severity: Severity::Error,
                    message: format!(
                        "component `{key}` has invalid name `{}` (must be kebab-case)",
                        comp.component.name
                    ),
                    node: Some(key.to_string()),
                });
            }
        }
        // Pattern names are human-readable (e.g. "All persistent state uses Redis")
        // and intentionally differ from the kebab-case filename key. No key-vs-name
        // check — only content checks (empty name/description) apply.
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

        let mut components = HashMap::new();
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

        let mut decisions = HashMap::new();
        decisions.insert(
            "use-jwt".into(),
            DecisionFile {
                decision: Decision {
                    component: "auth".into(),
                    choice: "JWT with DPoP".into(),
                    reason: "Stateless".into(),
                    alternatives: vec![],
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
                    created: ts(),
                },
            },
        );

        InMemoryGraph::build(&index, components, decisions, HashMap::new())
    }

    /// Graph with a DependsOn chain: d-a → d-b → d-c (depth 3 test).
    fn dep_chain_graph() -> InMemoryGraph {
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
                    name: "d-a".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: "a".into(),
                },
                NodeEntry {
                    name: "d-b".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: "b".into(),
                },
                NodeEntry {
                    name: "d-c".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: "c2".into(),
                },
                NodeEntry {
                    name: "d-d".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: "d".into(),
                },
            ],
            edges: vec![
                EdgeEntry {
                    from: "d-a".into(),
                    to: "comp".into(),
                    kind: EdgeKind::BelongsTo,
                },
                EdgeEntry {
                    from: "d-b".into(),
                    to: "comp".into(),
                    kind: EdgeKind::BelongsTo,
                },
                EdgeEntry {
                    from: "d-c".into(),
                    to: "comp".into(),
                    kind: EdgeKind::BelongsTo,
                },
                EdgeEntry {
                    from: "d-d".into(),
                    to: "comp".into(),
                    kind: EdgeKind::BelongsTo,
                },
                EdgeEntry {
                    from: "d-a".into(),
                    to: "d-b".into(),
                    kind: EdgeKind::DependsOn,
                },
                EdgeEntry {
                    from: "d-b".into(),
                    to: "d-c".into(),
                    kind: EdgeKind::DependsOn,
                },
                EdgeEntry {
                    from: "d-c".into(),
                    to: "d-d".into(),
                    kind: EdgeKind::DependsOn,
                },
            ],
        };
        let mut components = HashMap::new();
        components.insert(
            "comp".into(),
            ComponentFile {
                component: Component {
                    name: "comp".into(),
                    description: String::new(),
                },
            },
        );
        let mut decisions = HashMap::new();
        for name in ["d-a", "d-b", "d-c", "d-d"] {
            decisions.insert(
                name.into(),
                DecisionFile {
                    decision: Decision {
                        component: "comp".into(),
                        choice: format!("Choice {name}"),
                        reason: format!("Reason {name}"),
                        alternatives: vec![],
                        created: ts(),
                    },
                },
            );
        }
        InMemoryGraph::build(&index, components, decisions, HashMap::new())
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
        assert_eq!(targets[0].as_ref(), "database");
    }

    #[test]
    fn connects_from_returns_reverse() {
        let g = test_graph();
        let sources = g.connects_from("database");
        assert_eq!(sources.len(), 2);
        let names: HashSet<&str> = sources.iter().map(|a| a.as_ref()).collect();
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

    // ── transitive_deps ──────────────────────────────────────────────────

    #[test]
    fn transitive_deps_follows_chain() {
        let g = dep_chain_graph();
        let deps = g.transitive_deps("d-a");
        let names: Vec<&str> = deps.iter().map(|a| a.as_ref()).collect();
        assert!(names.contains(&"d-b"));
        assert!(names.contains(&"d-c"));
        assert!(names.contains(&"d-d"));
    }

    #[test]
    fn transitive_deps_max_depth_3() {
        // d-a → d-b → d-c → d-d. From d-a: depths 1,2,3 → all found.
        let g = dep_chain_graph();
        let deps = g.transitive_deps("d-a");
        assert_eq!(deps.len(), 3);
    }

    #[test]
    fn transitive_deps_respects_depth_limit() {
        // Add a 5th decision d-e at depth 4 from d-a.
        let mut index = GraphIndex {
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
                    name: "d-a".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: "1".into(),
                },
                NodeEntry {
                    name: "d-b".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: "2".into(),
                },
                NodeEntry {
                    name: "d-c".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: "3".into(),
                },
                NodeEntry {
                    name: "d-d".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: "4".into(),
                },
                NodeEntry {
                    name: "d-e".into(),
                    kind: NodeKind::Decision,
                    tags: vec![],
                    hash: "5".into(),
                },
            ],
            edges: vec![
                EdgeEntry {
                    from: "d-a".into(),
                    to: "comp".into(),
                    kind: EdgeKind::BelongsTo,
                },
                EdgeEntry {
                    from: "d-b".into(),
                    to: "comp".into(),
                    kind: EdgeKind::BelongsTo,
                },
                EdgeEntry {
                    from: "d-c".into(),
                    to: "comp".into(),
                    kind: EdgeKind::BelongsTo,
                },
                EdgeEntry {
                    from: "d-d".into(),
                    to: "comp".into(),
                    kind: EdgeKind::BelongsTo,
                },
                EdgeEntry {
                    from: "d-e".into(),
                    to: "comp".into(),
                    kind: EdgeKind::BelongsTo,
                },
                EdgeEntry {
                    from: "d-a".into(),
                    to: "d-b".into(),
                    kind: EdgeKind::DependsOn,
                },
                EdgeEntry {
                    from: "d-b".into(),
                    to: "d-c".into(),
                    kind: EdgeKind::DependsOn,
                },
                EdgeEntry {
                    from: "d-c".into(),
                    to: "d-d".into(),
                    kind: EdgeKind::DependsOn,
                },
                EdgeEntry {
                    from: "d-d".into(),
                    to: "d-e".into(),
                    kind: EdgeKind::DependsOn,
                },
            ],
        };
        let _ = &mut index; // suppress unused_mut
        let mut decisions = HashMap::new();
        let mut components = HashMap::new();
        components.insert(
            "comp".into(),
            ComponentFile {
                component: Component {
                    name: "comp".into(),
                    description: String::new(),
                },
            },
        );
        for name in ["d-a", "d-b", "d-c", "d-d", "d-e"] {
            decisions.insert(
                name.into(),
                DecisionFile {
                    decision: Decision {
                        component: "comp".into(),
                        choice: format!("C-{name}"),
                        reason: format!("R-{name}"),
                        alternatives: vec![],
                        created: ts(),
                    },
                },
            );
        }
        let g = InMemoryGraph::build(&index, components, decisions, HashMap::new());
        let deps = g.transitive_deps("d-a");
        // Depth 1: d-b, depth 2: d-c, depth 3: d-d. d-e is depth 4 → excluded.
        assert_eq!(deps.len(), 3);
        let names: Vec<&str> = deps.iter().map(|a| a.as_ref()).collect();
        assert!(names.contains(&"d-b"));
        assert!(names.contains(&"d-c"));
        assert!(names.contains(&"d-d"));
        assert!(!names.contains(&"d-e"));
    }

    #[test]
    fn transitive_deps_empty_for_leaf() {
        let g = dep_chain_graph();
        assert!(g.transitive_deps("d-d").is_empty());
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
        let mut components = HashMap::new();
        components.insert(
            "comp".into(),
            ComponentFile {
                component: Component {
                    name: "comp".into(),
                    description: String::new(),
                },
            },
        );
        let mut decisions = HashMap::new();
        for n in ["d1", "d2"] {
            decisions.insert(
                n.into(),
                DecisionFile {
                    decision: Decision {
                        component: "comp".into(),
                        choice: n.into(),
                        reason: n.into(),
                        alternatives: vec![],
                        created: ts(),
                    },
                },
            );
        }
        let mut patterns = HashMap::new();
        patterns.insert(
            "my-pattern".into(),
            PatternFile {
                pattern: Pattern {
                    name: "my-pattern".into(),
                    description: "test".into(),
                },
            },
        );

        let g = InMemoryGraph::build(&index, components, decisions, patterns);
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
        assert_eq!(forward[0].0.as_ref(), "database");

        let reverse: Vec<_> = edges
            .iter()
            .filter(|(_, _, d)| *d == Direction::Reverse)
            .collect();
        assert_eq!(reverse.len(), 1);
        assert_eq!(reverse[0].0.as_ref(), "use-jwt");
    }

    #[test]
    fn edges_involving_empty_for_unknown() {
        let g = test_graph();
        assert!(g.edges_involving("nonexistent").is_empty());
    }

    // ── would_cycle ──────────────────────────────────────────────────────

    #[test]
    fn would_cycle_detects_direct() {
        let g = dep_chain_graph();
        // d-b already depends on d-c. Adding d-c → d-b would cycle.
        assert!(g.would_cycle("d-c", "d-b"));
    }

    #[test]
    fn would_cycle_detects_transitive() {
        let g = dep_chain_graph();
        // d-a → d-b → d-c → d-d. Adding d-d → d-a would cycle.
        assert!(g.would_cycle("d-d", "d-a"));
    }

    #[test]
    fn would_cycle_self_loop() {
        let g = dep_chain_graph();
        assert!(g.would_cycle("d-a", "d-a"));
    }

    #[test]
    fn would_cycle_allows_valid() {
        let g = dep_chain_graph();
        // d-a → d-b → d-c → d-d. Adding d-a → d-d is a shortcut, not a cycle:
        // from d-d there are no outgoing DependsOn edges, so d-a is unreachable.
        assert!(!g.would_cycle("d-a", "d-d"));
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
        let g = InMemoryGraph::build(&index, HashMap::new(), HashMap::new(), HashMap::new());
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
        let g = InMemoryGraph::build(&index, HashMap::new(), HashMap::new(), HashMap::new());
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
        let g = InMemoryGraph::build(&index, HashMap::new(), HashMap::new(), HashMap::new());
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
        let g = InMemoryGraph::build(&index, HashMap::new(), HashMap::new(), HashMap::new());
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
        let g = InMemoryGraph::build(&index, HashMap::new(), HashMap::new(), HashMap::new());
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
        let g = InMemoryGraph::build(&index, HashMap::new(), HashMap::new(), HashMap::new());
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
        let mut decisions = HashMap::new();
        for n in ["d1", "d2"] {
            decisions.insert(
                n.into(),
                DecisionFile {
                    decision: Decision {
                        component: "project".into(),
                        choice: n.into(),
                        reason: n.into(),
                        alternatives: vec![],
                        created: ts(),
                    },
                },
            );
        }
        let g = InMemoryGraph::build(&index, HashMap::new(), decisions, HashMap::new());
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
        let mut decisions = HashMap::new();
        decisions.insert(
            "bad".into(),
            DecisionFile {
                decision: Decision {
                    component: "project".into(),
                    choice: String::new(),
                    reason: "ok".into(),
                    alternatives: vec![],
                    created: ts(),
                },
            },
        );
        let g = InMemoryGraph::build(&index, HashMap::new(), decisions, HashMap::new());
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
        let mut decisions = HashMap::new();
        decisions.insert(
            "bad".into(),
            DecisionFile {
                decision: Decision {
                    component: "project".into(),
                    choice: "ok".into(),
                    reason: "   ".into(),
                    alternatives: vec![],
                    created: ts(),
                },
            },
        );
        let g = InMemoryGraph::build(&index, HashMap::new(), decisions, HashMap::new());
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
        let mut components = HashMap::new();
        components.insert(
            "auth".into(),
            ComponentFile {
                component: Component {
                    name: "WRONG".into(),
                    description: String::new(),
                },
            },
        );
        let g = InMemoryGraph::build(&index, components, HashMap::new(), HashMap::new());
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
        let mut components = HashMap::new();
        components.insert(
            "Bad_Name".into(),
            ComponentFile {
                component: Component {
                    name: "Bad_Name".into(),
                    description: String::new(),
                },
            },
        );
        let g = InMemoryGraph::build(&index, components, HashMap::new(), HashMap::new());
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
        let mut components = HashMap::new();
        for (k, v) in &g.components {
            components.insert(k.to_string(), v.clone());
        }
        let mut decisions = HashMap::new();
        for (k, v) in &g.decisions {
            decisions.insert(k.to_string(), v.clone());
        }
        let g2 = InMemoryGraph::build(&idx, components, decisions, HashMap::new());

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
        let mut decisions = HashMap::new();
        decisions.insert(
            "dec".into(),
            DecisionFile {
                decision: Decision {
                    component: "comp".into(),
                    choice: "test".into(),
                    reason: "test".into(),
                    alternatives: vec![],
                    created: ts(),
                },
            },
        );
        let mut components = HashMap::new();
        components.insert(
            "comp".into(),
            ComponentFile {
                component: Component {
                    name: "comp".into(),
                    description: String::new(),
                },
            },
        );
        let g = InMemoryGraph::build(&index, components, decisions, HashMap::new());
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
        let mut decisions = HashMap::new();
        decisions.insert(
            "dec".into(),
            DecisionFile {
                decision: Decision {
                    component: "comp-a".into(),
                    choice: "test".into(),
                    reason: "test".into(),
                    alternatives: vec![],
                    created: ts(),
                },
            },
        );
        let mut components = HashMap::new();
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
        let g = InMemoryGraph::build(&index, components, decisions, HashMap::new());
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
        let mut decisions = HashMap::new();
        decisions.insert(
            "dec".into(),
            DecisionFile {
                decision: Decision {
                    component: "comp-b".into(), // does NOT match the edge target
                    choice: "test".into(),
                    reason: "test".into(),
                    alternatives: vec![],
                    created: ts(),
                },
            },
        );
        let mut components = HashMap::new();
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
        let g = InMemoryGraph::build(&index, components, decisions, HashMap::new());
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
        let mut patterns = HashMap::new();
        patterns.insert(
            "pat".into(),
            PatternFile {
                pattern: Pattern {
                    name: String::new(),
                    description: "something".into(),
                },
            },
        );
        let g = InMemoryGraph::build(&index, HashMap::new(), HashMap::new(), patterns);
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
        let mut decisions = HashMap::new();
        for n in ["d1", "d2"] {
            decisions.insert(
                n.into(),
                DecisionFile {
                    decision: Decision {
                        component: "project".into(),
                        choice: n.into(),
                        reason: n.into(),
                        alternatives: vec![],
                        created: ts(),
                    },
                },
            );
        }
        let mut patterns = HashMap::new();
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
        let g = InMemoryGraph::build(&index, HashMap::new(), decisions, patterns);
        let issues = g.validate();
        // Must NOT produce any warnings or errors about name mismatch.
        assert!(
            issues.is_empty(),
            "expected no issues, got: {:?}",
            issues.iter().map(|i| &i.message).collect::<Vec<_>>()
        );
    }
}
