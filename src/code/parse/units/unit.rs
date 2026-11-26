//! One unit of a span hash ([`Unit`]) and how it is hashed ([`Mark`]).

use tree_sitter::Node;

/// One unit of a span hash: a token over bytes `from..to`, or a group's
/// mark, which holds no byte (`from` and `to` are where it stands).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Unit {
    pub(super) from: usize,
    pub(super) to: usize,
    pub(super) mark: Mark,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Mark {
    Text,
    /// A string's delimiter, hashed as
    /// [`spelling::delimiter`](super::spelling::delimiter) spells it.
    Delimiter,
    /// A string's text, the holes in it left out and each escape in it
    /// spelled as [`spelling::escape`](super::spelling::escape) spells
    /// it.
    String,
    /// A number, hashed as [`spelling::literal`](super::spelling::literal)
    /// spells it.
    Literal,
    Open,
    Close,
}

impl Unit {
    /// The unit of `node`, one token spelled as `mark` says.
    pub(super) fn spelled(node: Node<'_>, mark: Mark) -> Self {
        Self {
            from: node.start_byte(),
            to: node.end_byte(),
            mark,
        }
    }

    pub(super) const fn text(from: usize, to: usize) -> Self {
        Self {
            from,
            to,
            mark: Mark::Text,
        }
    }

    pub(super) const fn mark(at: usize, mark: Mark) -> Self {
        Self {
            from: at,
            to: at,
            mark,
        }
    }
}
