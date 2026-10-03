use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};

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

/// Errors sort before warnings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Error,
    Warning,
}

/// What an [`Issue`] reports. A write may keep an error the graph already
/// had, matched on kind and subject, but never add one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum IssueKind {
    EdgeSourceMissing,
    EdgeTargetMissing,
    EdgeKindMismatch,
    SelfEdge,
    DuplicateEdge,
    PatternTooFewMembers,
    DependsOnCycle,
    MissingBelongsTo,
    MultipleBelongsTo,
    BelongsToMismatch,
    EmptyChoice,
    EmptyReason,
    DecisionComponentMissing,
    DecisionComponentInvalid,
    ComponentNameMismatch,
    ComponentNameInvalid,
    DecisionNameInvalid,
    EmptyPatternName,
    EmptyPatternDescription,
    NodeContentMissing,
    /// `graph.toml` is absent; the next load rebuilds it.
    IndexMissing,
    /// A node file's hash differs from the one in `graph.toml`.
    HashMismatch,
    /// `graph.toml` names a node whose file cannot be read.
    NodeFileMissing,
    /// Two index entries share a name; building the graph would keep one.
    DuplicateNode,
}

impl IssueKind {
    #[must_use]
    pub const fn severity(self) -> Severity {
        match self {
            Self::EmptyPatternDescription
            | Self::IndexMissing
            | Self::HashMismatch
            | Self::NodeFileMissing => Severity::Warning,
            Self::EdgeSourceMissing
            | Self::EdgeTargetMissing
            | Self::EdgeKindMismatch
            | Self::SelfEdge
            | Self::DuplicateEdge
            | Self::PatternTooFewMembers
            | Self::DependsOnCycle
            | Self::MissingBelongsTo
            | Self::MultipleBelongsTo
            | Self::BelongsToMismatch
            | Self::EmptyChoice
            | Self::EmptyReason
            | Self::DecisionComponentMissing
            | Self::DecisionComponentInvalid
            | Self::ComponentNameMismatch
            | Self::ComponentNameInvalid
            | Self::DecisionNameInvalid
            | Self::EmptyPatternName
            | Self::NodeContentMissing
            | Self::DuplicateNode => Severity::Error,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub kind: IssueKind,
    /// What the issue is about: a node name, an edge as `from -> to (kind)`,
    /// or a cycle's members, sorted and comma-separated. Stable across
    /// validations of the same graph, unlike `message`, which may carry counts.
    pub subject: String,
    pub message: String,
}

impl Issue {
    #[must_use]
    pub const fn severity(&self) -> Severity {
        self.kind.severity()
    }
}

// ── InMemoryGraph ───────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct InMemoryGraph {
    pub(crate) nodes: BTreeMap<Arc<str>, NodeMeta>,
    pub(crate) forward: BTreeMap<Arc<str>, Vec<Edge>>,
    pub(crate) reverse: BTreeMap<Arc<str>, Vec<Edge>>,
    pub(crate) components: BTreeMap<Arc<str>, Arc<ComponentFile>>,
    pub(crate) decisions: BTreeMap<Arc<str>, Arc<DecisionFile>>,
    pub(crate) patterns: BTreeMap<Arc<str>, Arc<PatternFile>>,
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
        // One allocation per node name, shared by every map below. A name
        // only an edge carries (a dangling edge, which validation reports)
        // gets its own. Each map is collected whole, which sorts once and
        // builds the tree in bulk instead of searching it per insert.
        let pool: BTreeMap<&str, Arc<str>> = index
            .nodes
            .iter()
            .map(|node| (node.name.as_str(), Arc::from(node.name.as_str())))
            .collect();
        let intern =
            |name: &str| -> Arc<str> { pool.get(name).cloned().unwrap_or_else(|| Arc::from(name)) };

        let nodes = index
            .nodes
            .iter()
            .map(|node| {
                let meta = NodeMeta {
                    kind: node.kind,
                    tags: node.tags.iter().map(|t| Arc::from(t.as_str())).collect(),
                    hash: node.hash.clone(),
                };
                (intern(&node.name), meta)
            })
            .collect();

        let mut outgoing = Vec::with_capacity(index.edges.len());
        let mut incoming = Vec::with_capacity(index.edges.len());
        for edge in &index.edges {
            let from = intern(&edge.from);
            let to = intern(&edge.to);
            outgoing.push((
                from.clone(),
                Edge {
                    target: to.clone(),
                    kind: edge.kind,
                },
            ));
            incoming.push((
                to,
                Edge {
                    target: from,
                    kind: edge.kind,
                },
            ));
        }
        let forward = group_by_node(outgoing);
        let reverse = group_by_node(incoming);

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

    /// Export as a sorted [`GraphIndex`]. `rebuilt` is the stamp of the index
    /// this one replaces, carried over unchanged.
    #[must_use]
    pub fn to_index(&self, rebuilt: Option<DateTime<Utc>>) -> GraphIndex {
        let nodes = self
            .nodes
            .iter()
            .map(|(name, meta)| NodeEntry {
                name: name.to_string(),
                kind: meta.kind,
                tags: meta.tags.iter().map(|t| t.to_string()).collect(),
                hash: meta.hash.clone(),
            })
            .collect();

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

        let mut index = GraphIndex {
            version: 1,
            rebuilt,
            nodes,
            edges,
        };
        index.sort();
        index
    }
}

// ── Tests ───────────────────────────────────────────────────────────────

/// Edges keyed by the node they leave or reach, each list in index order.
fn group_by_node(mut edges: Vec<(Arc<str>, Edge)>) -> BTreeMap<Arc<str>, Vec<Edge>> {
    // Stable, so edges of one node keep their index order.
    edges.sort_by(|a, b| a.0.cmp(&b.0));
    let mut grouped: Vec<(Arc<str>, Vec<Edge>)> = Vec::new();
    for (node, edge) in edges {
        match grouped.last_mut() {
            Some((last, list)) if *last == node => list.push(edge),
            _ => grouped.push((node, vec![edge])),
        }
    }
    grouped.into_iter().collect()
}

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
        let idx = g.to_index(None);

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
        let idx = g.to_index(None);

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

        let idx2 = g2.to_index(None);
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
