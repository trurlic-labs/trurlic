//! What the walk keeps of each ancestor of the node it is at, and what a
//! captured node waits for the walk to reach it with: the test mark, the
//! visibility marks and the label in force, each inherited from the
//! parent unless the node's own captures state one.

use tree_sitter::Node;

use super::SpanNode;
use crate::code::index::Visibility;

/// A node a `@visibility.<name>` capture marks: its bytes and the
/// visibility.
#[derive(Debug, Clone, Copy)]
pub(super) struct VisibilityMark {
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) visibility: Visibility,
}

/// What a captured node waits for the walk to reach it with.
#[derive(Debug, Clone, Copy)]
pub(super) enum Waiting {
    Definition(usize),
    Name(usize),
    Test,
    Visibility(Visibility),
    Within(Visibility),
    After(Visibility),
    /// A label among its children holds for its later siblings too.
    Through,
}

impl Waiting {
    /// The candidate a `@definition` capture waits for.
    pub(super) const fn definition(&self) -> Option<usize> {
        match self {
            Self::Definition(at) => Some(*at),
            Self::Name(_)
            | Self::Test
            | Self::Visibility(_)
            | Self::Within(_)
            | Self::After(_)
            | Self::Through => None,
        }
    }
}

/// What the walk keeps of an ancestor of the node it is at.
#[derive(Debug, Clone, Copy)]
pub(super) struct Frame {
    pub(super) kind: u16,
    pub(super) node: SpanNode,
    pub(super) test: bool,
    pub(super) visibility: Option<VisibilityMark>,
    pub(super) within: Option<Visibility>,
    /// The label in force for its children: the last one among them seen
    /// so far, else the one in force for it.
    pub(super) after: Option<Visibility>,
    /// Whether a label among its children holds past it (a `#if` arm's).
    through: bool,
}

impl Frame {
    /// The frame of `node` under `parent`: test code when its parent is
    /// or a test mark waited for it, under its own visibility marks of
    /// each form or else its parent's innermost (of two
    /// `@visibility.within` marks of the node, the one [`Visibility`]
    /// orders first), and under the label in force for its parent's
    /// children.
    pub(super) fn of(node: Node<'_>, parent: Option<Self>, waiting: &[Waiting]) -> Self {
        let span = SpanNode::of(node);
        let mut frame = Self {
            kind: node.kind_id(),
            node: span,
            test: parent.is_some_and(|parent| parent.test),
            visibility: parent.and_then(|parent| parent.visibility),
            within: parent.and_then(|parent| parent.within),
            after: parent.and_then(|parent| parent.after),
            through: false,
        };
        let mut within = None;
        for waited in waiting {
            match *waited {
                Waiting::Test => frame.test = true,
                Waiting::Visibility(visibility) => {
                    frame.visibility = Some(VisibilityMark {
                        start: span.start,
                        end: span.end,
                        visibility,
                    });
                }
                Waiting::Within(visibility) => {
                    within =
                        Some(within.map_or(visibility, |other: Visibility| other.min(visibility)));
                }
                Waiting::Through => frame.through = true,
                Waiting::Definition(_) | Waiting::Name(_) | Waiting::After(_) => {}
            }
        }
        frame.within = within.or(frame.within);
        frame
    }
}

/// Set `label` in force for the later children of the last of `frames`,
/// the parent of a node a label captured, and, while that frame lets a
/// label through, for the later children of the one above it.
pub(super) fn set_label(frames: &mut [Frame], label: Visibility) {
    for frame in frames.iter_mut().rev() {
        frame.after = Some(label);
        if !frame.through {
            return;
        }
    }
}
