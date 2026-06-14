//! Graph query methods for [`InMemoryGraph`].
//!
//! Read-only traversals: decision lookup, component adjacency, pattern
//! membership, and transitive dependency BFS. Separated from the core
//! graph module for navigability — the query surface is ~180 lines
//! across 15 methods and does not affect the build or validation paths.

use std::collections::HashSet;
use std::sync::Arc;

use super::graph::{Direction, Edge, InMemoryGraph};
use super::schema::{DecisionFile, EdgeKind, PatternFile};

// ── Query methods ────────────────────────────────────────────────────────

impl InMemoryGraph {
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
        if let Some(edges) = self.forward.get(component) {
            for e in edges.iter().filter(|e| e.kind == EdgeKind::ConnectsTo) {
                connected.insert(e.target.as_ref());
            }
        }
        if let Some(edges) = self.reverse.get(component) {
            for e in edges.iter().filter(|e| e.kind == EdgeKind::ConnectsTo) {
                connected.insert(e.target.as_ref());
            }
        }

        let mut seen: HashSet<&str> = HashSet::new();
        let mut result = Vec::new();
        for conn in connected {
            if let Some(edges) = self.reverse.get(conn) {
                for e in edges.iter().filter(|e| e.kind == EdgeKind::BelongsTo) {
                    if seen.insert(e.target.as_ref())
                        && let Some(dec) = self.decisions.get(&e.target)
                    {
                        result.push((&e.target, dec.as_ref()));
                    }
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
}

// ── Private helpers ─────────────────────────────────────────────────────

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

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::schema::*;
    use crate::store::testing::{arc_map, test_graph, ts};
    use std::collections::BTreeMap;

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
                        attribution: Attribution::User,
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
                        attribution: Attribution::User,
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
                        attribution: Attribution::User,
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
}
