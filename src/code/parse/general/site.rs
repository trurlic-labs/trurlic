//! Where the walk found a definition: the node its pattern captured and
//! its span, by byte and node id.

use tree_sitter::Node;

/// A node the walk passed: its id and its bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SpanNode {
    pub(super) id: usize,
    pub(super) start: usize,
    pub(super) end: usize,
}

impl SpanNode {
    pub(super) fn of(node: Node<'_>) -> Self {
        Self {
            id: node.id(),
            start: node.start_byte(),
            end: node.end_byte(),
        }
    }

    /// Whether its bytes hold `other`'s.
    pub(super) const fn holds(&self, other: Self) -> bool {
        self.start <= other.start && other.end <= self.end
    }
}

/// A definition's span: from node `first`'s start to node `last`'s end,
/// one node, or a run of siblings its node closes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Extent {
    pub(super) first: usize,
    pub(super) last: usize,
    pub(super) start: usize,
    pub(super) end: usize,
}

impl Extent {
    /// Whether its bytes hold `start..end`.
    pub(super) const fn holds(&self, start: usize, end: usize) -> bool {
        self.start <= start && end <= self.end
    }
}

/// Where a definition stands: the node its pattern captured, which
/// places it, and its span.
#[derive(Debug, Clone, Copy)]
pub(super) struct Site {
    pub(super) node: SpanNode,
    pub(super) span: Extent,
}
