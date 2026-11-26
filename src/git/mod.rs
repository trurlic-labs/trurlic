//! The `git` command line, run as a child process: the one way trurlic asks
//! git anything.
//!
//! Every invocation names its directory with `-C` and clears the variables
//! through which the caller's environment can point git at another
//! repository or change what the arguments mean: trurlic runs inside a
//! project's git hooks, where git exports `GIT_DIR` and `GIT_INDEX_FILE`
//! for the repository that runs the hook. Paths come from `-z` output, or
//! from `rev-parse`, which prints one path and a line break, so no name is
//! quoted or cut at a newline. Every child's stdout is read to a stated
//! bound, and none reads the caller's stdin (`process.rs`).
//!
//! The questions about the project's repository: the layout, that is the
//! top of a directory's work tree and of the one a git directory serves,
//! the common dir and the worktree list (`layout.rs`); which files the
//! index tracks and which an ignore rule matches (`files.rs`); the merge
//! base, `HEAD` and what changed since a commit (`revisions.rs`); and its
//! history ([`renames`]). "No work tree" is an answer, `None`, never a parsed
//! message; a missing binary is [`Error::GitMissing`](crate::Error). None
//! of them runs inside a graph directory: the graph's own repository is
//! [`graph`]'s alone.

mod encoding;
mod files;
mod layout;
mod process;
mod revisions;
#[cfg(test)]
mod testing;

pub(crate) mod graph;
pub(crate) mod renames;

pub(crate) use files::{ignored, is_ignored, tracked};
pub(crate) use layout::{
    DotGit, Head, Worktree, common_dir, dot_git, dot_git_in, git_dir, toplevel, work_tree_of,
    worktrees,
};
pub(crate) use revisions::{changed_since, head, merge_base};
