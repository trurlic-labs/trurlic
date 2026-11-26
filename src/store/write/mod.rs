//! The ways into the graph and every write to it: a read under the shared
//! lock, a [`Writer`] under the exclusive one, `init` and `migrate`, which
//! take the lock themselves, and the commit they all end in. A commit is
//! staged, made durable and journaled before anything is renamed into
//! place, and under `shared` placement it ends with a commit to the graph's
//! own git repository (`history.rs`); recovery finishes what a stopped
//! writer left, under the exclusive lock.

mod access;
mod commit;
mod create;
mod durable;
mod history;
mod journal;
mod lock;
mod message;
mod migrate;
mod recovery;

pub(crate) use access::Writer;
pub(in crate::store) use commit::PendingWrite;
pub(crate) use durable::sync_dir;
#[cfg(any(test, feature = "bench"))]
pub(in crate::store) use history::History;
pub(crate) use history::HistoryFinding;
pub(super) use lock::StoreLock;
pub(in crate::store) use message::{CommitMessage, Tool};
