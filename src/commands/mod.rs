mod component;
mod decision;
mod gc;
mod init;
mod install;
mod migrate;
mod pattern;
mod query;
mod serve;

pub use component::{
    add_component, add_connection, remove_component, remove_connection, rename_component,
};
pub(crate) use decision::parse_code_ref_arg;
pub use decision::{decide, remove_agent_decisions, remove_decision};
pub(crate) use gc::{AggressiveConfirm, resolve_aggressive_confirm};
pub use gc::{GcExecution, GcScope, gc};
pub use init::init;
pub(crate) use install::install;
pub(crate) use migrate::migrate;
pub(crate) use pattern::remove_pattern;
pub(crate) use query::{check, query_file, status};
pub use serve::serve;

use std::path::Path;

use crate::Result;
use crate::console::diag;
use crate::store::{ProjectState, Store, Writer};

/// Whether a mutating command should preview its plan or actually write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DryRun {
    Yes,
    No,
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn warn_on_issues(state: &ProjectState) {
    let issues = state.validate();
    let errors = issues
        .iter()
        .filter(|i| i.severity() == crate::store::graph::Severity::Error)
        .count();
    if errors > 0 {
        diag!(
            "warning: .trurlic/ has {errors} consistency issue(s) \u{2014} run `trurlic check` for details"
        );
    }
}

/// Load the graph for a read-only command and warn on any consistency
/// issues. The load waits only while a writer holds the lock.
pub(crate) fn open_store(cwd: &Path) -> Result<ProjectState> {
    let state = Store::discover(cwd)?.load_state()?;
    warn_on_issues(&state);
    Ok(state)
}

/// Begin the write of a mutating command and warn on any consistency issues
/// of the graph it loaded under the lock.
pub(crate) fn begin_write(store: &Store) -> Result<Writer<'_>> {
    let writer = store.begin_write()?;
    warn_on_issues(writer.state());
    Ok(writer)
}
