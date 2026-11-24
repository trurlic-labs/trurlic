//! What git says about where a graph sits, gathered for the store to
//! judge: the files its index tracks and its ignore rules match, and the
//! repository's main worktree with the graphs its worktrees carry. Git runs
//! in the directory that holds the graph, never inside it, where a graph
//! kept in a repository of its own would answer for that one.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::discovery::graph_in;
use crate::git::{self, Head, Worktree};
use crate::store::{LinkedGraph, MainWorktree, Placement, PlacementFacts, Store, WorkTreeFacts};
use crate::{Error, Result};

/// The facts `ProjectState::placement_issues` judges for the graph of
/// `store`.
pub(super) fn facts(store: &Store) -> Result<PlacementFacts> {
    let (parent, name) = split(store)?;
    if git::toplevel(parent)?.is_none() {
        return Ok(PlacementFacts::NoWorkTree);
    }
    let tracked = under(name, &git::tracked(parent, name)?);
    let ignored = under(name, &git::ignored(parent, name)?);
    let main = main_worktree(store.root(), &git::worktrees(parent)?)?;
    Ok(PlacementFacts::WorkTree(WorkTreeFacts {
        tracked,
        ignored,
        main,
    }))
}

/// The paths under the graph directory of `store`, relative to it, that
/// the index of the repository holding that directory tracks; `None` when
/// no work tree holds it.
pub(super) fn tracked(store: &Store) -> Result<Option<BTreeSet<PathBuf>>> {
    let (parent, name) = split(store)?;
    if git::toplevel(parent)?.is_none() {
        return Ok(None);
    }
    Ok(Some(under(name, &git::tracked(parent, name)?)))
}

/// The directory that holds the graph and the graph directory's name in
/// it, which git takes as a pathspec.
fn split(store: &Store) -> Result<(&Path, &Path)> {
    let root = store.root();
    match (root.parent(), root.file_name()) {
        (Some(parent), Some(name)) => Ok((parent, Path::new(name))),
        _ => Err(Error::Validation(format!(
            "{}: the graph directory has no parent",
            root.display()
        ))),
    }
}

/// `listing`, paths git printed relative to the graph's parent, as paths
/// relative to the graph directory `name`.
fn under(name: &Path, listing: &[PathBuf]) -> BTreeSet<PathBuf> {
    listing
        .iter()
        .filter_map(|path| path.strip_prefix(name).ok())
        .map(Path::to_path_buf)
        .collect()
}

/// The main worktree as it bears on the graph at `graph`: whether its
/// `.trurlic` leads to this graph, and if so the other graphs the linked
/// worktrees carry, else whether it holds a shared graph of its own.
fn main_worktree(graph: &Path, worktrees: &[Worktree]) -> Result<MainWorktree> {
    let Some((main, linked)) = worktrees.split_first() else {
        return Ok(MainWorktree::Bare);
    };
    if main.head == Head::Bare {
        return Ok(MainWorktree::Bare);
    }
    let main_graph = graph_in(&main.path)?;
    if main_graph.as_deref() == Some(graph) {
        let mut others = Vec::new();
        for worktree in linked
            .iter()
            .filter(|w| !w.prunable && w.head != Head::Bare)
        {
            if let Some(other) = graph_in(&worktree.path)?
                && other != graph
            {
                others.push(LinkedGraph {
                    graph: other,
                    // clone: the fact outlives git's listing.
                    worktree: worktree.path.clone(),
                    branch: match &worktree.head {
                        // clone: the fact outlives git's listing.
                        Head::Branch(branch) => Some(branch.clone()),
                        Head::Detached | Head::Bare => None,
                    },
                });
            }
        }
        return Ok(MainWorktree::Here { linked: others });
    }
    let shared = match main_graph {
        // clone: the store owns its root; the fact keeps the path.
        Some(other) if Store::at(other.clone()).stated_placement()? == Placement::Shared => {
            Some(other)
        }
        Some(_) | None => None,
    };
    Ok(MainWorktree::Elsewhere {
        // clone: the fact outlives git's listing.
        top: main.path.clone(),
        shared,
    })
}
