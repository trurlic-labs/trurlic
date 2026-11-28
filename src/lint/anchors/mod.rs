//! The anchor findings: each anchor of a decision that does not plainly
//! resolve in the calling tree, judged from that tree's code facts and,
//! when the caller asked the repository's git ([`Asked`]), the facts of
//! its other live worktrees.
//!
//! The verdict follows the cause. An anchor whose file is gone or defines
//! nothing its symbol names is [`Missing`](finding::Missing): its code moved, went, or is not
//! written yet. On a decision never accepted it is planned; on a decision
//! ever accepted (an accepted one, or a proposed revision of one, all of
//! whose anchors are judged as the accepted decision's, since the accepted
//! text keeps none) it is pending when another worktree that changed its
//! file since its line forked holds what it names, and stale when none
//! does. Only the repository's git tells those two apart, so with git not
//! asked such an anchor is unresolved, and a planned one is no finding. An
//! anchor that can never resolve as spelled, a symbol out of the grammar
//! or on a directory, is malformed whatever the decision's status; one on
//! a file trurlic cannot read, of a kind no resolver reads or that cannot
//! be indexed, is unverified, whatever the status: neither is asked of
//! another worktree. A symbol that names definitions of several paths is
//! ambiguous; a resolved symbol that is one plain name, not the whole path
//! of what it names, which [`WEAK_FILES`] or more files of its kind define,
//! is weak. A symbol that names one path's definitions of several kinds, a
//! field and its getter, is no finding: the index makes them one place on
//! purpose. Findings sort by kind, the most severe first, then by decision
//! and anchor. A stale anchor carries its repair when the tree and git's
//! history show one ([`repair`]).

mod finding;
mod judge;
pub(crate) mod repair;
mod trees;

pub(crate) use finding::{Checked, Finding, Kind, KindName, Repair, Severity, Unasked, Worktree};
pub(crate) use judge::{WEAK_FILES, findings, in_tree, unasked};
pub(crate) use repair::{Prior, Renames};
pub(crate) use trees::{Asked, Decisions, Elsewhere, Here};

#[cfg(test)]
mod testing;
#[cfg(test)]
mod tests;
