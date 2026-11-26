//! What a batch did: each op's outcome, the anchors its repairs replaced
//! and left, its warnings, and how many files it wrote and removed.

use super::ops::{Field, OpKind};
use super::refusal::Rule;
use super::warning::{PatternNote, Warning};
use crate::store::model::{Anchor, StatusName};

/// What a batch wrote: each op's outcome, in order, its warnings, and the
/// files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Applied {
    pub(crate) ops: Vec<Done>,
    /// The warnings listed: each `warn` pattern warns at its first match
    /// per op, and the first [`MAX_WARNINGS_PER_OP`] of the origin's and of
    /// each op's are listed, in the order of the ops and the patterns.
    ///
    /// [`MAX_WARNINGS_PER_OP`]: crate::store::limits::MAX_WARNINGS_PER_OP
    pub(crate) warnings: Vec<Warning>,
    /// The origin's warnings not listed.
    pub(crate) more_origin_warnings: usize,
    /// The message of each pattern a listed warning names.
    pub(crate) patterns: Vec<PatternNote>,
    /// Files written, a rename's new file included.
    pub(crate) written: usize,
    /// Files removed, a rename's old file included.
    pub(crate) removed: usize,
    /// Each anchor the `repair_anchors` ops replaced, in the order they
    /// replaced them.
    pub(crate) repaired: Vec<Repaired>,
    /// Each repair of an op with no `repairs` that the bar did not admit,
    /// in the order they were judged.
    pub(crate) left: Vec<LeftRepair>,
}

/// An anchor a `repair_anchors` op replaced in a decision: `from` was held
/// and `to` is, `from` dropped when the decision held `to` already.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Repaired {
    pub(crate) decision: String,
    pub(crate) from: Anchor,
    pub(crate) to: Anchor,
}

/// A repair of an op with no `repairs` the store did not write, since the
/// bar refused it or could not judge it: the decision holds `from` still.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LeftRepair {
    pub(crate) repaired: Repaired,
    pub(crate) rule: Rule,
}

/// What one op did, named, never echoed: the node, the nodes it names
/// besides, the fields it changed, and how many other files it rewrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Done {
    pub(crate) op: OpKind,
    /// The node the op names first; a merge's survivor; the first decision
    /// a repair changed, empty when it changed none.
    pub(crate) name: String,
    /// A rename's new name.
    pub(crate) to: Option<String>,
    /// A merge's absorbed decision.
    pub(crate) absorbed: Option<String>,
    /// Where a remove pointed the edges that pointed at its decision.
    pub(crate) repoint_to: Option<String>,
    /// A revise's fields whose values changed, in the order of the file's
    /// keys.
    pub(crate) changed: Vec<Field>,
    /// The decision's status, when the op changed it.
    pub(crate) status: Option<StatusName>,
    /// Files of other nodes the op rewrote: dependents repointed, a
    /// component's decisions and connections renamed, the decisions a
    /// repair changed after its first.
    pub(crate) rewrote: usize,
    /// The op's warnings not listed in [`Applied::warnings`].
    pub(crate) more_warnings: usize,
}

impl Done {
    pub(super) fn of(op: OpKind, name: String) -> Self {
        Self {
            op,
            name,
            to: None,
            absorbed: None,
            repoint_to: None,
            changed: Vec::new(),
            status: None,
            rewrote: 0,
            more_warnings: 0,
        }
    }
}
