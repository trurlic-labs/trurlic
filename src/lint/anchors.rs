//! The anchor findings: each anchor of a decision that does not plainly
//! resolve in the calling tree, judged from that tree's code facts and,
//! when the caller asked the repository's git ([`Asked`]), the facts of
//! its other live worktrees.
//!
//! The verdict follows the cause. An anchor whose file is gone or defines
//! nothing its symbol names is [`Missing`]: its code moved, went, or is not
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

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::code::{CodeFacts, Resolution};
use crate::store::{Anchor, Decision, Status};

pub(crate) mod repair;

pub(crate) use repair::{Prior, Renames, Repair};

/// The files of its kind that define a name, at least, for an anchor whose
/// symbol is that name alone, resolved through a suffix, to be weak.
pub(crate) const WEAK_FILES: usize = 10;

/// A live worktree of the repository other than the calling tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Worktree {
    /// Where it is read, as a message shows it: its top, joined with the
    /// calling tree's place in its own worktree.
    pub(crate) path: String,
    /// `None` when its HEAD is detached.
    pub(crate) branch: Option<String>,
}

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

/// The decisions whose anchors are judged, by name.
pub(crate) type Decisions<'s> = [(&'s str, &'s Decision)];

/// Why an anchor names nothing here when its code moved, went, or is not
/// written yet: nothing is at its path, or its file defines nothing its
/// symbol names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Missing(Resolution);

/// Why an anchor can never resolve as spelled: a symbol out of the grammar
/// of its file's kind, or a symbol on a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Broken(Resolution);

/// Why trurlic cannot say what an anchor names: its symbol is on a kind of
/// file no resolver reads, or on a file that cannot be read or indexed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Unread(Resolution);

/// What a [`Resolution`] makes of an anchor before its decision's status
/// and the other worktrees are asked.
enum Class {
    Resolved,
    Missing(Missing),
    Broken(Broken),
    Unread(Unread),
    Ambiguous(usize),
}

impl Class {
    fn of(resolution: &Resolution) -> Self {
        // clone: the class keeps the cause the finding names, a variant
        // that holds no definition.
        let cause = || resolution.clone();
        match resolution {
            Resolution::Resolved(_) => Self::Resolved,
            Resolution::FileMissing | Resolution::SymbolMissing => Self::Missing(Missing(cause())),
            Resolution::Malformed(_) | Resolution::SymbolOnDirectory => {
                Self::Broken(Broken(cause()))
            }
            Resolution::NoResolver | Resolution::Unindexable(_) => Self::Unread(Unread(cause())),
            Resolution::Ambiguous(paths) => Self::Ambiguous(*paths),
        }
    }
}

impl fmt::Display for Missing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl fmt::Display for Broken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl fmt::Display for Unread {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}, so what it names is not verified", self.0)
    }
}

/// What a finding says of its anchor, a missing one judged as `V` says:
/// [`Checked`] with the repository's git asked, [`Unasked`] without.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Kind<V> {
    /// It names nothing here, for want of its file or its symbol.
    Missing(Missing, V),
    /// It can never resolve as spelled.
    Malformed(Broken),
    /// Its file is one trurlic cannot read.
    Unverified(Unread),
    /// Its symbol names definitions of this many paths, which a fuller
    /// spelling tells apart.
    Ambiguous(usize),
    /// Its symbol is one name, not the whole path of what it names, that
    /// this many files of its kind define.
    Weak(usize),
}

/// What `check`, which runs the repository's git, makes of a missing
/// anchor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Checked {
    /// No other worktree holds it, on a decision ever accepted. `repair` is
    /// what it is repaired as, when the tree and git's history show one.
    Stale { repair: Option<Repair> },
    /// It names what `0` holds, on a decision ever accepted.
    Pending(Worktree),
    /// It is on a decision never accepted.
    Planned,
}

/// What `status`, which runs no git in the project, makes of a missing
/// anchor of a decision ever accepted: it does not resolve in this tree,
/// stale or pending alike.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Unasked;

/// A [`Kind`] without its data, in the order findings sort by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum KindName {
    Stale,
    Unresolved,
    Malformed,
    Ambiguous,
    Unverified,
    Weak,
    Pending,
    Planned,
}

/// How `check` reports a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Severity {
    /// Fails `check`.
    Error,
    Warning,
    Note,
}

impl KindName {
    /// The kinds of a judgement with git not asked.
    pub(crate) const UNASKED: [Self; 5] = [
        Self::Unresolved,
        Self::Malformed,
        Self::Ambiguous,
        Self::Unverified,
        Self::Weak,
    ];

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Stale => "stale",
            Self::Unresolved => "unresolved",
            Self::Malformed => "malformed",
            Self::Ambiguous => "ambiguous",
            Self::Unverified => "unverified",
            Self::Weak => "weak",
            Self::Pending => "pending",
            Self::Planned => "planned",
        }
    }
}

/// What a judgement makes of a missing anchor, and how it says so.
pub(crate) trait Verdict: fmt::Debug {
    fn name(&self) -> KindName;

    /// The cause, as a sentence after the anchor.
    fn describe(&self, cause: &Missing, f: &mut fmt::Formatter<'_>) -> fmt::Result;
}

impl Verdict for Checked {
    fn name(&self) -> KindName {
        match self {
            Self::Stale { .. } => KindName::Stale,
            Self::Pending(_) => KindName::Pending,
            Self::Planned => KindName::Planned,
        }
    }

    fn describe(&self, cause: &Missing, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Planned => fmt::Display::fmt(cause, f),
            Self::Stale { repair } => {
                fmt::Display::fmt(cause, f)?;
                match repair {
                    Some(repair) => write!(f, "; repair: `{}`", repair.to),
                    None => Ok(()),
                }
            }
            Self::Pending(worktree) => {
                let path = &worktree.path;
                write!(f, "{cause} here; it resolves in the worktree at {path}")?;
                match &worktree.branch {
                    Some(branch) => write!(f, " on branch `{branch}`"),
                    None => f.write_str(", whose HEAD is detached"),
                }
            }
        }
    }
}

impl Verdict for Unasked {
    fn name(&self) -> KindName {
        KindName::Unresolved
    }

    fn describe(&self, cause: &Missing, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{cause} in this tree")
    }
}

impl<V: Verdict> Kind<V> {
    pub(crate) fn name(&self) -> KindName {
        match self {
            Self::Missing(_, verdict) => verdict.name(),
            Self::Malformed(_) => KindName::Malformed,
            Self::Unverified(_) => KindName::Unverified,
            Self::Ambiguous(_) => KindName::Ambiguous,
            Self::Weak(_) => KindName::Weak,
        }
    }
}

impl Kind<Checked> {
    /// How `check` reports it: a stale, malformed or ambiguous anchor
    /// fails it, an unverified or weak one is a warning, a pending or
    /// planned one a note.
    pub(crate) const fn severity(&self) -> Severity {
        match self {
            Self::Missing(_, Checked::Stale { .. }) | Self::Malformed(_) | Self::Ambiguous(_) => {
                Severity::Error
            }
            Self::Unverified(_) | Self::Weak(_) => Severity::Warning,
            Self::Missing(_, Checked::Pending(_) | Checked::Planned) => Severity::Note,
        }
    }
}

/// The cause, as a sentence after the anchor.
impl<V: Verdict> fmt::Display for Kind<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(cause, verdict) => verdict.describe(cause, f),
            Self::Malformed(cause) => cause.fmt(f),
            Self::Unverified(cause) => cause.fmt(f),
            Self::Ambiguous(paths) => write!(f, "its symbol names definitions of {paths} paths"),
            Self::Weak(files) => {
                write!(
                    f,
                    "its symbol is one name, defined in {files} files of its kind"
                )
            }
        }
    }
}

/// One anchor of one decision that does not plainly resolve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Finding<'s, V> {
    pub(crate) decision: &'s str,
    pub(crate) anchor: &'s Anchor,
    pub(crate) kind: Kind<V>,
}

impl<V: Verdict> fmt::Display for Finding<'_, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (anchor, decision) = (self.anchor, self.decision);
        let name = self.kind.name().as_str();
        write!(
            f,
            "anchor `{anchor}` of `{decision}` is {name}: {}",
            self.kind
        )
    }
}

/// What each anchor of the decisions resolves to in the calling tree, each
/// anchor resolved once however many decisions name it.
#[derive(Debug)]
pub(crate) struct Here<'s, 'f> {
    facts: &'f CodeFacts,
    resolutions: BTreeMap<&'s Anchor, Resolution>,
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
/// anchor git's history shows ([`repair`]).
#[derive(Debug)]
pub(crate) struct Asked<'s> {
    read: Vec<Elsewhere>,
    /// The place in `read` of the worktree that holds each anchor held.
    holders: BTreeMap<&'s Anchor, usize>,
    stale: BTreeSet<&'s Anchor>,
    repairs: BTreeMap<&'s Anchor, Repair>,
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
    fn holder(&self, anchor: &Anchor) -> Option<&Worktree> {
        let at = *self.holders.get(anchor)?;
        self.read.get(at).map(|tree| &tree.worktree)
    }

    /// The files of the stale anchors with nothing at their path here:
    /// those git's history is asked where they went.
    pub(crate) fn gone(&self, here: &Here<'s, '_>) -> BTreeSet<&'s str> {
        self.stale
            .iter()
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

/// The findings of `decisions`' anchors, a missing anchor judged by
/// `missing` from whether its decision was ever accepted, sorted.
fn judged<'s, V: Verdict>(
    decisions: &Decisions<'s>,
    here: &Here<'s, '_>,
    missing: impl Fn(&Anchor, bool) -> Option<V>,
) -> Vec<Finding<'s, V>> {
    let spread = here.facts.spread(here.resolutions.keys().copied());
    let judge = |anchor: &Anchor, status: &Status| -> Option<Kind<V>> {
        Some(match Class::of(here.resolutions.get(anchor)?) {
            Class::Missing(cause) => Kind::Missing(cause, missing(anchor, status.ever_accepted())?),
            Class::Broken(cause) => Kind::Malformed(cause),
            Class::Unread(cause) => Kind::Unverified(cause),
            Class::Ambiguous(count) => Kind::Ambiguous(count),
            Class::Resolved => {
                let files = *spread.get(anchor)?;
                return (files >= WEAK_FILES).then_some(Kind::Weak(files));
            }
        })
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

#[cfg(test)]
mod tests;
