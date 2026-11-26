//! What an anchor finding says: the cause of a missing, malformed or
//! unread anchor, the [`Kind`] of a finding with the verdict of the git it
//! was or was not judged by, its severity, the [`Worktree`] a pending
//! anchor names, and the [`Repair`] a stale anchor carries.

use std::fmt;

use crate::code::{Code, Resolution};
use crate::store::Anchor;

/// A stale anchor's repair: the anchor it is replaced by, and the code that
/// anchor names in the tree it was judged in, which the write holds it to
/// again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Repair {
    pub(crate) to: Anchor,
    pub(crate) code: Code,
}

/// Why an anchor names nothing here when its code moved, went, or is not
/// written yet: nothing is at its path, or its file defines nothing its
/// symbol names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Missing(pub(super) Resolution);

/// Why an anchor can never resolve as spelled: a symbol out of the grammar
/// of its file's kind, or a symbol on a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Broken(pub(super) Resolution);

/// Why trurlic cannot say what an anchor names: its symbol is on a kind of
/// file no resolver reads, or on a file that cannot be read or indexed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Unread(pub(super) Resolution);

/// A live worktree of the repository other than the calling tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Worktree {
    /// Where it is read, as a message shows it: its top, joined with the
    /// calling tree's place in its own worktree.
    pub(crate) path: String,
    /// `None` when its HEAD is detached.
    pub(crate) branch: Option<String>,
}

/// What a [`Resolution`] makes of an anchor before its decision's status
/// and the other worktrees are asked.
pub(super) enum Class {
    Resolved,
    Missing(Missing),
    Broken(Broken),
    Unread(Unread),
    Ambiguous(usize),
}

impl Class {
    pub(super) fn of(resolution: &Resolution) -> Self {
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
