use std::collections::{BTreeMap, HashMap};
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
        // Intern every name that appears in nodes or edges. Borrow keys
        // from `index` (which outlives this function) to avoid cloning
        // Strings into the pool map.
        let mut pool: HashMap<&str, Arc<str>> = HashMap::with_capacity(index.nodes.len());
        for node in &index.nodes {
            pool.entry(node.name.as_str())
                .or_insert_with(|| Arc::from(node.name.as_str()));
        }
        for edge in &index.edges {
            pool.entry(edge.from.as_str())
                .or_insert_with(|| Arc::from(edge.from.as_str()));
            pool.entry(edge.to.as_str())
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

        let mut forward: HashMap<Arc<str>, Vec<Edge>> = HashMap::with_capacity(index.nodes.len());
        let mut reverse: HashMap<Arc<str>, Vec<Edge>> = HashMap::with_capacity(index.nodes.len());
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

        let edge_count: usize = self.forward.values().map(Vec::len).sum();
        let mut edges: Vec<EdgeEntry> = Vec::with_capacity(edge_count);
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

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::schema::*;
    use crate::store::testing::test_graph;

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
}
