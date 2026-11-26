//! The graph's own repository: the git repository at `.trurlic/.git` that
//! holds a shared graph's history, which only trurlic writes. This is the
//! one module that runs git inside a graph directory; discovery and the
//! placement facts ask git only about the directory that holds a graph.
//!
//! Every call but `init` names the repository and its work tree
//! (`--git-dir`, `--work-tree`), so git discovers nothing: a missing `.git` is git's
//! error, never the project's repository answering in its place. No
//! configuration of the system's or the user's is read, and the settings
//! of the repository's own that run code or change bytes are overridden
//! (`isolation.rs`), so neither a caller nor a configuration can break a
//! commit or point it elsewhere. Commits are made as `trurlic` with the
//! dates git reads from the clock.
//!
//! What reaches the index, and every blob of the object store, is `index`'s: files staged
//! as their bytes on disk, blobs written from a write's staged files, index
//! entries set from blob ids, every one through no filter.

mod index;
mod isolation;
mod oid;
mod repository;

pub(crate) use oid::Oid;
pub(crate) use repository::{Committed, Repository, Status};
