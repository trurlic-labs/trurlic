//! What the anchors resolve to: in the calling tree ([`Here`]), and, with
//! the repository's git asked, in its other worktrees and by its history
//! ([`Asked`]). The facts handed in, never read.

use std::collections::{BTreeMap, BTreeSet};

use crate::code::{CodeFacts, Resolution};
use crate::store::{Anchor, Decision};

use super::finding::{Class, Repair, Worktree};

/// Another worktree whose work tree differs from where its line forked
/// from the calling tree's at the files of some missing anchors, and the
/// code facts of those files there and at the fork.
#[derive(Debug)]
pub(crate) struct Elsewhere {
    pub(crate) worktree: Worktree,
    pub(crate) facts: CodeFacts,
    /// The facts of those files at the commit where the two lines forked;
    /// `None` when they share no commit.
    pub(crate) fork: Option<CodeFacts>,
}

/// The decisions whose anchors are judged, by name.
pub(crate) type Decisions<'s> = [(&'s str, &'s Decision)];

/// What each anchor of the decisions resolves to in the calling tree, each
/// anchor resolved once however many decisions name it.
#[derive(Debug)]
pub(crate) struct Here<'s, 'f> {
    pub(super) facts: &'f CodeFacts,
    pub(super) resolutions: BTreeMap<&'s Anchor, Resolution>,
}

impl<'s, 'f> Here<'s, 'f> {
    pub(crate) fn of(decisions: &Decisions<'s>, facts: &'f CodeFacts) -> Self {
        let anchors: BTreeSet<&Anchor> = decisions
            .iter()
            .flat_map(|(_, decision)| &decision.anchors)
            .collect();
        let resolutions = anchors
            .into_iter()
            .map(|anchor| (anchor, facts.resolve(anchor)))
            .collect();
        Self { facts, resolutions }
    }

    /// The anchors of `decisions` ever accepted that are missing here:
    /// stale unless another worktree holds them, so those the other
    /// worktrees are asked about. A planned, malformed or unverified anchor
    /// is not among them.
    pub(crate) fn unresolved(&self, decisions: &Decisions<'s>) -> BTreeSet<&'s Anchor> {
        decisions
            .iter()
            .filter(|(_, decision)| decision.status.ever_accepted())
            .flat_map(|(_, decision)| &decision.anchors)
            .filter(|anchor| {
                let resolution = self.resolutions.get(anchor);
                resolution
                    .is_some_and(|resolution| matches!(Class::of(resolution), Class::Missing(_)))
            })
            .collect()
    }
}

/// What the repository's git says beyond the calling tree, which `check`
/// reads and `status` does not: the other worktrees read, in the shell's
/// order, with the first that holds each anchor [`Here::unresolved`] gave,
/// and those none holds, which are stale; and the repair of each stale
/// anchor git's history shows ([`repair`](super::repair)).
#[derive(Debug)]
pub(crate) struct Asked<'s> {
    read: Vec<Elsewhere>,
    /// The place in `read` of the worktree that holds each anchor held.
    holders: BTreeMap<&'s Anchor, usize>,
    stale: BTreeSet<&'s Anchor>,
    pub(super) repairs: BTreeMap<&'s Anchor, Repair>,
}

impl<'s> Asked<'s> {
    /// `unresolved`, each stale until a worktree read holds it, with no
    /// repair known yet.
    pub(crate) fn new(unresolved: BTreeSet<&'s Anchor>) -> Self {
        Self {
            read: Vec::new(),
            holders: BTreeMap::new(),
            stale: unresolved,
            repairs: BTreeMap::new(),
        }
    }

    /// The anchors no worktree read so far holds: those the next one is
    /// read for.
    pub(crate) fn stale(&self) -> &BTreeSet<&'s Anchor> {
        &self.stale
    }

    /// Read `tree`, the next other worktree: it holds each anchor still
    /// stale that names one definition there, or several, and named none
    /// at the fork. Code the fork held is code the calling tree's line
    /// removed since, which another worktree's older copy never brings
    /// back, whatever else that worktree changed in the file.
    pub(crate) fn read(&mut self, tree: Elsewhere) {
        let at = self.read.len();
        self.stale.retain(|&anchor| {
            let at_fork = tree.fork.as_ref().is_some_and(|fork| holds(fork, anchor));
            let held = holds(&tree.facts, anchor) && !at_fork;
            if held {
                self.holders.insert(anchor, at);
            }
            !held
        });
        self.read.push(tree);
    }

    /// The worktree that holds `anchor`, the first read that does.
    pub(super) fn holder(&self, anchor: &Anchor) -> Option<&Worktree> {
        let at = *self.holders.get(anchor)?;
        self.read.get(at).map(|tree| &tree.worktree)
    }

    /// The files of the stale file anchors with nothing at their path
    /// here: those git's history is asked where they went.
    pub(crate) fn gone(&self, here: &Here<'s, '_>) -> BTreeSet<&'s str> {
        self.stale
            .iter()
            .filter(|anchor| anchor.symbol().is_none())
            .filter(|anchor| here.resolutions.get(*anchor) == Some(&Resolution::FileMissing))
            .map(|anchor| anchor.file())
            .collect()
    }

    /// These findings with `repairs`, what git's history shows of the
    /// stale anchors.
    pub(crate) fn with_repairs(self, repairs: BTreeMap<&'s Anchor, Repair>) -> Self {
        Self { repairs, ..self }
    }
}

/// Whether `anchor` names what `facts` hold: one definition, or several.
fn holds(facts: &CodeFacts, anchor: &Anchor) -> bool {
    match facts.resolve(anchor) {
        Resolution::Resolved(_) | Resolution::Ambiguous(_) => true,
        Resolution::FileMissing
        | Resolution::SymbolMissing
        | Resolution::SymbolOnDirectory
        | Resolution::NoResolver
        | Resolution::Unindexable(_)
        | Resolution::Malformed(_) => false,
    }
}
