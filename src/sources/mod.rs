//! The shell around the code index and the anchor findings: it walks and
//! reads the code root, and, for `check`, the files of the anchors
//! missing in it that the other live worktrees changed since their line
//! forked from the calling tree's, and what git's history says of the
//! stale anchors' files and their repairs; reads and fills the index's
//! cache through the [`Store`](crate::store::Store); and hands the facts to `code` and `lint`
//! as data. `tools` (`status`, `apply`) and `commands` (`check`) stand on
//! it. A caller holds no lock of the store while it runs: the cache's
//! write takes the exclusive lock when it is free, and the caller's own
//! shared lock would keep it busy.
//!
//! A file is read in one bounded read, refused past
//! [`MAX_SOURCE_FILE_BYTES`](source::MAX_SOURCE_FILE_BYTES) and when it is
//! no regular file, and indexed from the cache when an entry holds its key
//! (`source.rs`); a C or C++ file's key holds the part of the tree's blank
//! sets it writes, which its lexical pass, cached by content, says
//! (`lexical.rs`). Every read of a call, of the calling tree, another
//! worktree or git's history, and the recheck of a repair under the write
//! lock, goes through one `Reads`, made by one function from the calling
//! tree's walk and headers (`reads.rs`), so a file is judged against the
//! same sets whichever read takes it. The findings are gathered in
//! `findings.rs`, the repairs of the stale ones, and their recheck under
//! the write lock, in `repair/`. Indexing runs in
//! parallel, on threads the call starts and ends (`indexer.rs`); a file
//! that vanishes between the walk and the read is absent. The misses of
//! every tree a call reads are written to the cache once, after the
//! call's own work, under the exclusive lock when it is free: a busy lock
//! skips the write, since the cache is never authoritative.

mod blobs;
mod cache;
mod findings;
mod indexer;
mod lexical;
mod reads;
mod repair;
mod source;
mod walk;
mod worktrees;

pub(crate) use findings::{checked_findings, tree_findings};
pub(crate) use repair::{Seen, still_holding};

#[cfg(test)]
mod tests;
