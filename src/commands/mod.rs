mod check;
mod fmt;
mod init;
mod install;
mod migrate;
mod placement;
mod query;
mod serve;

pub(crate) use check::check;
pub(crate) use fmt::fmt;
pub(crate) use init::init;
pub(crate) use install::install;
pub(crate) use migrate::migrate;
pub(crate) use query::{query_file, status};
pub(crate) use serve::serve;

use std::path::Path;

use crate::Result;
use crate::console::diag;
use crate::discovery::discover;
use crate::store::{ProjectState, STORE_DIR, Store};

/// Whether a mutating command should preview its plan or actually write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DryRun {
    Yes,
    No,
}

/// Load the graph for a read-only command and warn when it has issues. The
/// load waits only while a writer holds the lock.
pub(crate) fn open_store(cwd: &Path) -> Result<(Store, ProjectState)> {
    let store = discover(cwd)?;
    let state = store.load_state()?;
    let issues = state.validate().len();
    if issues > 0 {
        diag!(
            "warning: {} has {issues} issue(s); run `trurlic check` for details",
            graph_name(&store)
        );
    }
    Ok((store, state))
}

/// The graph directory as a message names it: `.trurlic/` when it is the
/// code root's own, else its path, since a tree that reaches the graph
/// through a symlink or a linked worktree holds no such directory.
pub(crate) fn graph_name(store: &Store) -> String {
    if store.is_local() {
        format!("{STORE_DIR}/")
    } else {
        format!("{}/", store.root().display())
    }
}
