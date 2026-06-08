//! Diff two [`ProjectState`] snapshots into WebSocket events.
//!
//! Used by the file watcher: on external change, reload state from disk,
//! diff against the previous snapshot, broadcast only the delta. Falls
//! back to `FullReload` when the change is too large or ambiguous.

use std::collections::{HashMap, HashSet};

use serde::Serialize;
use serde_json::Value;

use crate::store::ProjectState;
use crate::store::schema::{EdgeKind, NodeEntry, NodeKind};

/// Maximum number of granular events before falling back to `full_reload`.
/// A `git checkout` that rewrites 50 files would produce hundreds of events —
/// a single `full_reload` is cheaper for the client to process.
const MAX_GRANULAR_EVENTS: usize = 50;

// ── WebSocket event types ──────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum WsEvent {
    NodeAdded {
        node: NodeSnapshot,
    },
    NodeUpdated {
        name: String,
        changes: Value,
    },
    NodeRemoved {
        name: String,
    },
    EdgeAdded {
        edge: EdgeSnapshot,
    },
    EdgeRemoved {
        from: String,
        to: String,
        kind: String,
    },
    FullReload,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct NodeSnapshot {
    pub name: String,
    pub kind: String,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct EdgeSnapshot {
    pub from: String,
    pub to: String,
    pub kind: String,
}

// ── Diffing ────────────────────────────────────────────────────────────────

/// Compare two states and produce WebSocket events for the delta.
/// Returns `[FullReload]` if the diff is too large.
pub(crate) fn diff_states(old: &ProjectState, new: &ProjectState) -> Vec<WsEvent> {
    let mut events = Vec::new();

    // Index old and new nodes by name for O(1) lookup.
    let old_nodes: HashMap<&str, &NodeEntry> = old
        .graph_index
        .nodes
        .iter()
        .map(|n| (n.name.as_str(), n))
        .collect();
    let new_nodes: HashMap<&str, &NodeEntry> = new
        .graph_index
        .nodes
        .iter()
        .map(|n| (n.name.as_str(), n))
        .collect();

    // Removed nodes.
    for &name in old_nodes.keys() {
        if !new_nodes.contains_key(name) {
            events.push(WsEvent::NodeRemoved {
                name: name.to_string(),
            });
        }
    }

    // Added or updated nodes.
    for (&name, &new_node) in &new_nodes {
        match old_nodes.get(name) {
            None => events.push(WsEvent::NodeAdded {
                node: snapshot_node(new_node),
            }),
            Some(old_node) if old_node.hash != new_node.hash => {
                events.push(WsEvent::NodeUpdated {
                    name: name.to_string(),
                    changes: node_changes(old_node, new_node, new),
                });
            }
            _ => {}
        }
    }

    // Edge diffs — set comparison on (from, to, kind) triples.
    let old_edges: HashSet<(&str, &str, EdgeKind)> = old
        .graph_index
        .edges
        .iter()
        .map(|e| (e.from.as_str(), e.to.as_str(), e.kind))
        .collect();
    let new_edges: HashSet<(&str, &str, EdgeKind)> = new
        .graph_index
        .edges
        .iter()
        .map(|e| (e.from.as_str(), e.to.as_str(), e.kind))
        .collect();

    for &(from, to, kind) in old_edges.difference(&new_edges) {
        events.push(WsEvent::EdgeRemoved {
            from: from.to_string(),
            to: to.to_string(),
            kind: kind.as_str().to_string(),
        });
    }
    for &(from, to, kind) in new_edges.difference(&old_edges) {
        events.push(WsEvent::EdgeAdded {
            edge: EdgeSnapshot {
                from: from.to_string(),
                to: to.to_string(),
                kind: kind.as_str().to_string(),
            },
        });
    }

    if events.is_empty() {
        return events;
    }

    // Fall back to full_reload if the diff is too large.
    if events.len() > MAX_GRANULAR_EVENTS {
        return vec![WsEvent::FullReload];
    }

    events
}

fn snapshot_node(node: &NodeEntry) -> NodeSnapshot {
    NodeSnapshot {
        name: node.name.clone(),
        kind: node.kind.as_str().to_string(),
        tags: node.tags.clone(),
    }
}

/// Build a JSON object describing what changed in an updated node.
fn node_changes(old: &NodeEntry, new: &NodeEntry, state: &ProjectState) -> Value {
    let mut changes = serde_json::Map::new();

    if old.tags != new.tags {
        changes.insert(
            "tags".into(),
            serde_json::to_value(&new.tags).unwrap_or_default(),
        );
    }

    // For decisions, include the updated choice/reason if available.
    if new.kind == NodeKind::Decision
        && let Some(dec) = state.decisions.get(&new.name)
    {
        changes.insert("choice".into(), Value::String(dec.decision.choice.clone()));
        changes.insert("reason".into(), Value::String(dec.decision.reason.clone()));
    }

    Value::Object(changes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::schema::*;

    fn minimal_state(nodes: Vec<NodeEntry>, edges: Vec<EdgeEntry>) -> ProjectState {
        let mut state = crate::store::testing::empty_project_state();
        state.graph_index.nodes = nodes;
        state.graph_index.edges = edges;
        state.rebuild_graph();
        state
    }

    fn node(name: &str, kind: NodeKind, hash: &str) -> NodeEntry {
        NodeEntry {
            name: name.into(),
            kind,
            tags: vec![],
            hash: hash.into(),
        }
    }

    #[test]
    fn identical_states_produce_no_events() {
        let state = minimal_state(vec![node("auth", NodeKind::Component, "a")], vec![]);
        assert!(diff_states(&state, &state).is_empty());
    }

    #[test]
    fn added_node_produces_event() {
        let old = minimal_state(vec![], vec![]);
        let new = minimal_state(vec![node("auth", NodeKind::Component, "a")], vec![]);
        let events = diff_states(&old, &new);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], WsEvent::NodeAdded { node } if node.name == "auth"));
    }

    #[test]
    fn removed_node_produces_event() {
        let old = minimal_state(vec![node("auth", NodeKind::Component, "a")], vec![]);
        let new = minimal_state(vec![], vec![]);
        let events = diff_states(&old, &new);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], WsEvent::NodeRemoved { name } if name == "auth"));
    }

    #[test]
    fn updated_node_detected_by_hash() {
        let old = minimal_state(vec![node("auth", NodeKind::Component, "hash1")], vec![]);
        let new = minimal_state(vec![node("auth", NodeKind::Component, "hash2")], vec![]);
        let events = diff_states(&old, &new);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], WsEvent::NodeUpdated { name, .. } if name == "auth"));
    }

    #[test]
    fn added_edge_produces_event() {
        let n = vec![
            node("a", NodeKind::Component, "1"),
            node("b", NodeKind::Component, "2"),
        ];
        let old = minimal_state(n.clone(), vec![]);
        let new = minimal_state(
            n,
            vec![EdgeEntry {
                from: "a".into(),
                to: "b".into(),
                kind: EdgeKind::ConnectsTo,
            }],
        );
        let events = diff_states(&old, &new);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, WsEvent::EdgeAdded { .. }))
        );
    }

    #[test]
    fn large_diff_falls_back_to_full_reload() {
        let old = minimal_state(vec![], vec![]);
        let nodes: Vec<NodeEntry> = (0..60)
            .map(|i| node(&format!("n{i}"), NodeKind::Component, &format!("h{i}")))
            .collect();
        let new = minimal_state(nodes, vec![]);
        let events = diff_states(&old, &new);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], WsEvent::FullReload));
    }

    #[test]
    fn edge_diff_kind_is_snake_case() {
        let n = vec![
            node("a", NodeKind::Component, "1"),
            node("b", NodeKind::Component, "2"),
        ];
        let old = minimal_state(n.clone(), vec![]);
        let new = minimal_state(
            n,
            vec![EdgeEntry {
                from: "a".into(),
                to: "b".into(),
                kind: EdgeKind::ConnectsTo,
            }],
        );
        let events = diff_states(&old, &new);
        let edge_event = events
            .iter()
            .find_map(|e| match e {
                WsEvent::EdgeAdded { edge } => Some(edge),
                _ => None,
            })
            .expect("should have an EdgeAdded event");
        assert_eq!(
            edge_event.kind, "connects_to",
            "edge kind must be snake_case, not Debug-formatted CamelCase"
        );
    }

    #[test]
    fn removed_edge_kind_is_snake_case() {
        let n = vec![
            node("a", NodeKind::Component, "1"),
            node("b", NodeKind::Component, "2"),
        ];
        let old = minimal_state(
            n.clone(),
            vec![EdgeEntry {
                from: "a".into(),
                to: "b".into(),
                kind: EdgeKind::DependsOn,
            }],
        );
        let new = minimal_state(n, vec![]);
        let events = diff_states(&old, &new);
        let removed = events
            .iter()
            .find_map(|e| match e {
                WsEvent::EdgeRemoved { kind, .. } => Some(kind.as_str()),
                _ => None,
            })
            .expect("should have an EdgeRemoved event");
        assert_eq!(removed, "depends_on");
    }
}
