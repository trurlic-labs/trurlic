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
//! Another worktree may hold an anchor only when its line forked from the
//! calling tree's and changed the anchor's file since: `git merge-base`
//! of the two HEADs, once per worktree at another commit (a base that is
//! the worktree's own HEAD means it holds nothing the calling tree lacks),
//! then which of the missing paths its work tree differs in from the base
//! (`git diff --name-only`, uncommitted edits included). So a branch cut
//! before the calling tree deleted something, whose work tree still holds
//! it, never hides the deletion. A worktree on an unborn branch holds no
//! commit and is left out; on an unborn calling tree every path of a
//! worktree with a commit counts. A worktree git marks prunable, the bare
//! entry and one with no directory at the prefix are left out too. A git
//! failure, a calling tree git lists as no live worktree and a path that
//! cannot be made canonical leave the worktrees concerned unread, which
//! the caller reports as data.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::git::{self, DotGit, Head};
use crate::lint::anchors::Worktree;
use crate::{Error, Result};

/// The other worktrees whose line forked from the calling tree's, at the
/// code root's prefix, in git's order, and those git could not say enough
/// of.
#[derive(Debug, Default)]
pub(super) struct Others {
    pub(super) trees: Vec<Forked>,
    pub(super) unread: Vec<UnreadWorktree>,
}

/// Another worktree whose HEAD holds commits the calling tree lacks, and
/// where its line forked from the calling tree's: `None` when the calling
/// tree has no commit or the two share no history, so all of that
/// worktree is its own.
#[derive(Debug)]
pub(super) struct Forked {
    pub(super) worktree: Worktree,
    base: Option<String>,
}

impl Forked {
    /// Which of `paths`, relative to the code root, this worktree changed
    /// since the fork: all of them when the calling tree has no commit.
    pub(super) fn changed(&self, paths: &[&str]) -> Result<BTreeSet<String>> {
        match &self.base {
            Some(base) => git::changed_since(&self.worktree.path, base, paths),
            None => Ok(paths.iter().map(|&path| path.to_owned()).collect()),
        }
    }
}

/// Other worktrees not read for pending anchors, so an anchor stale here
/// may be pending there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnreadWorktree {
    /// `None` when git listed no worktree to read.
    pub(crate) worktree: Option<PathBuf>,
    pub(crate) cause: String,
}

impl UnreadWorktree {
    pub(super) fn of(worktree: Option<PathBuf>, error: &Error) -> Self {
        Self {
            worktree,
            cause: error.to_string(),
        }
    }
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

/// The other worktrees of the repository holding `code_root` whose line
/// forked from its own worktree's, the main worktree first.
pub(super) fn others(code_root: &Path) -> Others {
    listed(code_root).unwrap_or_else(|unread| Others {
        trees: Vec::new(),
        unread: vec![unread],
    })
}

/// A listed worktree, its top made canonical.
struct Listed {
    top: PathBuf,
    branch: Option<String>,
    commit: Option<String>,
}

fn listed(code_root: &Path) -> std::result::Result<Others, UnreadWorktree> {
    let unread = |error: Error| UnreadWorktree::of(None, &error);
    if git::dot_git(code_root).map_err(unread)? == DotGit::Absent {
        return Ok(Others::default());
    }
    let here = code_root.canonicalize().map_err(|source| UnreadWorktree {
        worktree: None,
        cause: format!("{} cannot be made canonical: {source}", code_root.display()),
    })?;
    let live = live(code_root).map_err(unread)?;
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
        match fork(code_root, tree, own) {
            Ok(Some(base)) if path.is_dir() => others.trees.push(Forked {
                worktree: Worktree {
                    path,
                    // clone: the finding outlives the listing.
                    branch: tree.branch.clone(),
                },
                base,
            }),
            Ok(_) => {}
            Err(error) => others.unread.push(UnreadWorktree::of(Some(path), &error)),
        }
    }
    Ok(others)
}

/// The calling tree's own worktree, of `live`: the one whose top is the
/// nearest at or above `here`, the canonical code root.
fn own<'l>(live: &'l [Listed], here: &Path) -> std::result::Result<&'l Listed, UnreadWorktree> {
    live.iter()
        .filter(|tree| here.starts_with(&tree.top))
        .max_by_key(|tree| tree.top.components().count())
        .ok_or_else(|| UnreadWorktree {
            worktree: None,
            cause: format!(
                "git lists no live worktree at or above {}; `git worktree repair` mends one \
                 that was moved",
                here.display()
            ),
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
        let top = worktree.path.canonicalize().map_err(|source| Error::Io {
            // clone: the error names the path the listing still holds.
            path: worktree.path.clone(),
            source,
        })?;
        listed.push(Listed {
            top,
            branch,
            commit: worktree.commit,
        });
    }
    Ok(listed)
}

/// Where `tree`'s line forked from `own`'s, when `tree`'s HEAD holds
/// commits `own`'s lacks: `Some(Some(base))`, or `Some(None)` when `own`
/// has no commit or they share none; `None` when it holds none: at the
/// same commit, at an ancestor of `own`'s (the base is its own HEAD), or
/// with no commit at all.
fn fork(code_root: &Path, tree: &Listed, own: &Listed) -> Result<Option<Option<String>>> {
    match (&tree.commit, &own.commit) {
        (None, _) => Ok(None),
        (Some(_), None) => Ok(Some(None)),
        (Some(theirs), Some(ours)) if theirs == ours => Ok(None),
        (Some(theirs), Some(ours)) => Ok(match git::merge_base(code_root, ours, theirs)? {
            Some(base) if base == *theirs => None,
            base => Some(base),
        }),
    }
}
