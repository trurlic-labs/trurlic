//! Cycles in the `depends_on` subgraph, found as its strongly connected
//! components (Tarjan).
//!
//! The search keeps its own stack on the heap, so a dependency chain of any
//! length runs in constant call-stack depth. A cycle is reported once, as the
//! sorted set of its members; a node that only leads into a cycle is not a
//! member. A one-node loop is a self-edge, which validation reports on its
//! own.

use std::collections::HashMap;
use std::slice;

use super::graph::{Edge, InMemoryGraph};
use super::schema::EdgeKind;

impl InMemoryGraph {
    /// Every `depends_on` cycle as its sorted members, the cycles sorted.
    #[must_use]
    pub(super) fn depends_on_cycles(&self) -> Vec<Vec<&str>> {
        let mut search = Search::default();
        for (root, edges) in &self.forward {
            let depends = edges.iter().any(|edge| edge.kind == EdgeKind::DependsOn);
            if depends && !search.visits.contains_key(root.as_ref()) {
                search.run(self, root);
            }
        }
        search.cycles.sort_unstable();
        search.cycles
    }
}

#[derive(Debug, Clone, Copy)]
enum Visit {
    /// On the component stack, with its discovery order.
    Open(usize),
    /// Assigned to a component.
    Closed,
}

/// A node whose successors the search is still walking.
struct Frame<'g> {
    node: &'g str,
    order: usize,
    /// The lowest discovery order reachable from `node` through nodes still
    /// open; `node` roots a component when this equals `order`.
    low: usize,
    successors: slice::Iter<'g, Edge>,
}

#[derive(Default)]
struct Search<'g> {
    /// Looked up, never iterated: the cycles are sorted on return.
    visits: HashMap<&'g str, Visit>,
    /// Open nodes in discovery order; a component is a suffix of it.
    open: Vec<&'g str>,
    cycles: Vec<Vec<&'g str>>,
}

impl<'g> Search<'g> {
    fn run(&mut self, graph: &'g InMemoryGraph, root: &'g str) {
        let mut frames = vec![self.enter(graph, root)];
        while let Some(frame) = frames.last_mut() {
            let next = frame
                .successors
                .by_ref()
                .find(|edge| edge.kind == EdgeKind::DependsOn);
            if let Some(edge) = next {
                match self.visits.get(edge.target.as_ref()).copied() {
                    None => frames.push(self.enter(graph, &edge.target)),
                    Some(Visit::Open(order)) => frame.low = frame.low.min(order),
                    Some(Visit::Closed) => {}
                }
            } else if let Some(done) = frames.pop() {
                if let Some(parent) = frames.last_mut() {
                    parent.low = parent.low.min(done.low);
                }
                if done.low == done.order {
                    self.close(done.node);
                }
            }
        }
    }

    fn enter(&mut self, graph: &'g InMemoryGraph, node: &'g str) -> Frame<'g> {
        let order = self.visits.len();
        self.visits.insert(node, Visit::Open(order));
        self.open.push(node);
        let successors = graph.forward.get(node).map_or(&[][..], Vec::as_slice);
        Frame {
            node,
            order,
            low: order,
            successors: successors.iter(),
        }
    }

    /// Pop the component rooted at `root` off the open stack.
    fn close(&mut self, root: &'g str) {
        let mut members = Vec::new();
        while let Some(node) = self.open.pop() {
            self.visits.insert(node, Visit::Closed);
            members.push(node);
            if node == root {
                break;
            }
        }
        if members.len() > 1 {
            members.sort_unstable();
            self.cycles.push(members);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::store::graph::Edge;
    use crate::store::schema::EdgeKind;
    use crate::store::testing::depends_on_graph;

    #[test]
    fn an_acyclic_graph_has_no_cycles() {
        let g = depends_on_graph(&[("a", "b"), ("a", "c"), ("b", "d"), ("c", "d")]);
        assert!(g.depends_on_cycles().is_empty());
    }

    #[test]
    fn disjoint_cycles_are_reported_apart_and_sorted() {
        let g = depends_on_graph(&[("y", "x"), ("x", "y"), ("b", "a"), ("a", "b"), ("x", "a")]);
        assert_eq!(g.depends_on_cycles(), [["a", "b"], ["x", "y"]]);
    }

    #[test]
    fn cycles_sharing_a_node_form_one_component() {
        let g = depends_on_graph(&[("a", "b"), ("b", "a"), ("b", "c"), ("c", "b")]);
        assert_eq!(g.depends_on_cycles(), [["a", "b", "c"]]);
    }

    #[test]
    fn a_self_edge_is_left_to_the_self_edge_check() {
        let g = depends_on_graph(&[("a", "a")]);
        assert!(g.depends_on_cycles().is_empty());
    }

    #[test]
    fn edges_of_other_kinds_close_no_cycle() {
        let mut g = depends_on_graph(&[("a", "b")]);
        g.forward.entry("b".into()).or_default().push(Edge {
            target: "a".into(),
            kind: EdgeKind::Constrains,
        });
        assert!(g.depends_on_cycles().is_empty());
    }

    /// A recursive search needs a call frame per link of the chain and
    /// overflows this thread's stack long before the chain ends.
    #[test]
    fn a_long_chain_runs_in_constant_stack() {
        const LINKS: usize = 50_000;
        let names: Vec<String> = (0..=LINKS).map(|i| format!("d{i}")).collect();
        let mut edges: Vec<(&str, &str)> = names
            .windows(2)
            .map(|pair| (pair[0].as_str(), pair[1].as_str()))
            .collect();
        edges.push((names[LINKS].as_str(), names[0].as_str()));
        let g = depends_on_graph(&edges);

        let members = std::thread::scope(|scope| {
            std::thread::Builder::new()
                .stack_size(64 * 1024)
                .spawn_scoped(scope, || g.depends_on_cycles().concat().len())
                .unwrap()
                .join()
                .unwrap()
        });
        assert_eq!(members, LINKS + 1);
    }
}
