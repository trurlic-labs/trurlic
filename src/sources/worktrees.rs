//! The repository's other worktrees that may hold what an anchor names
//! when the calling tree lacks it.
//!
//! Git is asked only for `check`, when an anchor of a decision ever
//! accepted names nothing in the calling tree for want of its file or its
//! symbol, and a `.git` is at or above the code root: `git worktree list`
//! once, run in the code root, never in a graph directory. The calling
//! tree's own worktree is the listed one whose top is the nearest at or
//! above the code root; the code root's place in it, its prefix, is where
//! every other worktree is read, so a graph in a subdirectory is read
//! against the same subdirectory there.
//!
//! One rule says at which paths another worktree may hold an anchor: those
//! its work tree differs in from the commit where its line forked from the
//! calling tree's, the merge base of the two HEADs (`git diff --name-only`
//! from that base, then the untracked files `git ls-files --others`
//! lists). So code written there and not committed, or not yet added,
//! counts whether that worktree is ahead, behind or at the calling tree's
//! commit, and a branch cut before the calling tree deleted something,
//! whose work tree still holds it, never hides the deletion. When the two
//! lines share no commit, either one on an unborn branch or their
//! histories unrelated, every path of that worktree counts. A worktree git
//! marks prunable, the bare entry and one with no directory at the prefix
//! are left out. A git failure, a calling tree git lists as no live
//! worktree and a path that cannot be made canonical leave the worktrees
//! concerned unread, which the caller reports as data.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::git::{self, DotGit, Head};
use crate::lint::anchors::Worktree;
use crate::{Error, Result};

/// The other worktrees, at the code root's prefix, in git's order, and
/// those git could not say enough of.
#[derive(Debug, Default)]
pub(super) struct Others {
    pub(super) trees: Vec<Forked>,
    pub(super) unread: Vec<UnreadWorktree>,
}

/// Another worktree and where its line forked from the calling tree's:
/// `None` when the two share no commit, so all of that worktree is its
/// own.
#[derive(Debug)]
pub(super) struct Forked {
    pub(super) worktree: Worktree,
    /// Where it is read, which `worktree` names as text.
    pub(super) top: PathBuf,
    base: Option<String>,
}

impl Forked {
    /// Which of `paths`, relative to the code root, this worktree's work
    /// tree differs in from the fork: all of them when there is none.
    pub(super) fn changed(&self, paths: &[&str]) -> Result<BTreeSet<String>> {
        match &self.base {
            Some(base) => git::changed_since(&self.top, base, paths),
            None => Ok(paths.iter().map(|&path| path.to_owned()).collect()),
        }
    }
}

/// Other worktrees not read for pending anchors, so an anchor stale here
/// may be pending there.
#[derive(Debug)]
pub(crate) struct UnreadWorktree {
    /// `None` when git listed no worktree to read.
    pub(crate) worktree: Option<PathBuf>,
    pub(crate) cause: UnreadCause,
}

/// Why other worktrees were not read.
#[derive(Debug, thiserror::Error)]
pub(crate) enum UnreadCause {
    /// A git call failed, or a path could not be made canonical.
    #[error(transparent)]
    Failed(#[from] Error),
    /// Git lists no live worktree at or above `here`, the canonical code
    /// root.
    #[error(
        "git lists no live worktree at or above {}; `git worktree repair` mends one that was \
         moved",
        here.display()
    )]
    NotListed { here: PathBuf },
}

impl fmt::Display for UnreadWorktree {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.worktree {
            Some(path) => write!(f, "the worktree at {} was not read", path.display())?,
            None => f.write_str("the other worktrees were not read")?,
        }
        write!(f, " for pending anchors: {}", self.cause)
    }
}

/// The other worktrees of the repository holding `code_root`, the main
/// worktree first.
pub(super) fn others(code_root: &Path) -> Others {
    listed(code_root).unwrap_or_else(|cause| Others {
        trees: Vec::new(),
        unread: vec![UnreadWorktree {
            worktree: None,
            cause,
        }],
    })
}

/// A listed worktree, its top made canonical.
struct Listed {
    top: PathBuf,
    branch: Option<String>,
    commit: Option<String>,
}

fn listed(code_root: &Path) -> std::result::Result<Others, UnreadCause> {
    if git::dot_git(code_root)? == DotGit::Absent {
        return Ok(Others::default());
    }
    let here = code_root.canonicalize().map_err(Error::io(code_root))?;
    let live = live(code_root)?;
    let own = own(&live, &here)?;
    let prefix = here.strip_prefix(&own.top).unwrap_or(Path::new(""));
    let mut others = Others::default();
    for tree in live.iter().filter(|tree| tree.top != own.top) {
        let path = match prefix.as_os_str().is_empty() {
            // clone: the worktree found outlives the listing; `join` of an
            // empty prefix would end the path with a separator.
            true => tree.top.clone(),
            false => tree.top.join(prefix),
        };
        if !path.is_dir() {
            continue;
        }
        match fork(code_root, tree, own) {
            Ok(base) => others.trees.push(Forked {
                worktree: Worktree {
                    path: path.display().to_string(),
                    // clone: the finding outlives the listing.
                    branch: tree.branch.clone(),
                },
                top: path,
                base,
            }),
            Err(error) => others.unread.push(UnreadWorktree {
                worktree: Some(path),
                cause: error.into(),
            }),
        }
    }
    Ok(others)
}

/// The calling tree's own worktree, of `live`: the one whose top is the
/// nearest at or above `here`, the canonical code root.
fn own<'l>(live: &'l [Listed], here: &Path) -> std::result::Result<&'l Listed, UnreadCause> {
    live.iter()
        .filter(|tree| here.starts_with(&tree.top))
        .max_by_key(|tree| tree.top.components().count())
        .ok_or_else(|| UnreadCause::NotListed {
            here: here.to_path_buf(),
        })
}

/// The worktrees of the repository holding `code_root` with a work tree:
/// neither prunable nor bare, their tops canonical; one whose top cannot
/// be made canonical is a failure, which leaves every worktree unread.
fn live(code_root: &Path) -> Result<Vec<Listed>> {
    let mut listed = Vec::new();
    for worktree in git::worktrees(code_root)? {
        let branch = match worktree.head {
            Head::Bare => continue,
            Head::Branch(branch) => Some(branch),
            Head::Detached => None,
        };
        if worktree.prunable {
            continue;
        }
        let top = worktree
            .path
            .canonicalize()
            .map_err(Error::io(&worktree.path))?;
        listed.push(Listed {
            top,
            branch,
            commit: worktree.commit,
        });
    }
    Ok(listed)
}

/// Where `tree`'s line forked from `own`'s: their merge base, the commit
/// itself when both are at one; `None` when they share no commit, either
/// one on an unborn branch or their histories unrelated.
fn fork(code_root: &Path, tree: &Listed, own: &Listed) -> Result<Option<String>> {
    match (&tree.commit, &own.commit) {
        // clone: the base outlives the listing.
        (Some(theirs), Some(ours)) if theirs == ours => Ok(Some(ours.clone())),
        (Some(theirs), Some(ours)) => git::merge_base(code_root, ours, theirs),
        // An unborn branch on either side: no commit to share.
        (None, _) | (_, None) => Ok(None),
    }
}
