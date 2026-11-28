//! The judgement: each anchor of the decisions made a [`Finding`] from
//! what it resolves to, with git asked or not, sorted by kind, decision and
//! anchor.

use crate::code::Resolution;
use crate::store::{Anchor, Status};

use super::finding::{Checked, Class, Finding, InTree, Kind, Unasked, Verdict};
use super::trees::{Asked, Decisions, Here};

/// The files of its kind that define a name, at least, for an anchor whose
/// symbol is that name alone, resolved through a suffix, to be weak.
pub(crate) const WEAK_FILES: usize = 10;

/// The findings of `decisions`' anchors with git asked, judged from what
/// they resolve to `here` and in the other worktrees `asked` read.
pub(crate) fn findings<'s>(
    decisions: &Decisions<'s>,
    here: &Here<'s, '_>,
    asked: &Asked<'s>,
) -> Vec<Finding<'s, Checked>> {
    judged(decisions, here, |anchor, accepted| {
        if !accepted {
            return Some(Checked::Planned);
        }
        Some(match asked.holder(anchor) {
            // clone: a finding outlives the facts it is judged from.
            Some(worktree) => Checked::Pending(worktree.clone()),
            None => Checked::Stale {
                // clone: each finding owns its repair, which decisions share.
                repair: asked.repairs.get(anchor).cloned(),
            },
        })
    })
}

/// The findings of `decisions`' anchors in the calling tree alone, git
/// not asked: a missing anchor of a decision ever accepted is unresolved,
/// and one of a decision never accepted no finding.
pub(crate) fn unasked<'s>(
    decisions: &Decisions<'s>,
    here: &Here<'s, '_>,
) -> Vec<Finding<'s, Unasked>> {
    judged(decisions, here, |_, accepted| accepted.then_some(Unasked))
}

/// The finding of an anchor of a decision of `status` that resolves to
/// `resolution` in the calling tree alone, as one file's anchors are read
/// with git not asked: as [`unasked`] judges it, a missing anchor of a
/// decision never accepted planned. `None` when it resolves.
pub(crate) fn in_tree(resolution: &Resolution, status: &Status) -> Option<Kind<InTree>> {
    unresolved(resolution, || {
        Some(match status.ever_accepted() {
            true => InTree::Unasked(Unasked),
            false => InTree::Planned,
        })
    })
}

/// The kind of an anchor that resolves to `resolution`, a missing one
/// judged by `missing`; `None` for one that resolves, or a missing one
/// `missing` makes no finding.
fn unresolved<V: Verdict>(
    resolution: &Resolution,
    missing: impl FnOnce() -> Option<V>,
) -> Option<Kind<V>> {
    Some(match Class::of(resolution) {
        Class::Missing(cause) => Kind::Missing(cause, missing()?),
        Class::Broken(cause) => Kind::Malformed(cause),
        Class::Unread(cause) => Kind::Unverified(cause),
        Class::Ambiguous(count) => Kind::Ambiguous(count),
        Class::Resolved => return None,
    })
}

/// The findings of `decisions`' anchors, a missing anchor judged by
/// `missing` from whether its decision was ever accepted, sorted.
fn judged<'s, V: Verdict>(
    decisions: &Decisions<'s>,
    here: &Here<'s, '_>,
    missing: impl Fn(&Anchor, bool) -> Option<V>,
) -> Vec<Finding<'s, V>> {
    let spread = here.facts.spread(here.resolutions.keys().copied());
    let judge = |anchor: &Anchor, status: &Status| -> Option<Kind<V>> {
        let resolution = here.resolutions.get(anchor)?;
        if let Resolution::Resolved(_) = resolution {
            let files = *spread.get(anchor)?;
            return (files >= WEAK_FILES).then_some(Kind::Weak(files));
        }
        unresolved(resolution, || missing(anchor, status.ever_accepted()))
    };
    let mut found: Vec<Finding<'s, V>> = decisions
        .iter()
        .flat_map(|&(name, decision)| {
            let judge = &judge;
            decision.anchors.iter().filter_map(move |anchor| {
                Some(Finding {
                    decision: name,
                    anchor,
                    kind: judge(anchor, &decision.status)?,
                })
            })
        })
        .collect();
    found.sort_by(|a, b| {
        (a.kind.name(), a.decision, a.anchor).cmp(&(b.kind.name(), b.decision, b.anchor))
    });
    found
}
