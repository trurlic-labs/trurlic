mod fmt;
mod init;
mod install;
mod migrate;
mod query;
mod serve;

pub(crate) use fmt::fmt;
pub(crate) use init::init;
pub(crate) use install::install;
pub(crate) use migrate::migrate;
pub(crate) use query::{check, query_file, status};
pub(crate) use serve::serve;

use std::path::Path;

use crate::Result;
use crate::console::diag;
use crate::store::{ProjectState, Store};

/// Whether a mutating command should preview its plan or actually write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DryRun {
    Yes,
    No,
}

/// Load the graph for a read-only command and warn when it has issues. The
/// load waits only while a writer holds the lock.
pub(crate) fn open_store(cwd: &Path) -> Result<ProjectState> {
    let state = Store::discover(cwd)?.load_state()?;
    let issues = state.validate().len();
    if issues > 0 {
        diag!("warning: .trurlic/ has {issues} issue(s); run `trurlic check` for details");
    }
    Ok(state)
}
