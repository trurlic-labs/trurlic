mod component;
mod decision;
mod gc;
mod init;
pub(crate) mod install;
mod map;
pub(crate) mod migrate;
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
pub use install::install;
pub use map::map;
pub use migrate::migrate;
pub(crate) use pattern::remove_pattern;
pub(crate) use query::{check, query_file, status};
pub use serve::serve;

use std::path::Path;

use crate::Result;
use crate::console::diag;
use crate::store::{self, ProjectState, Store};

/// Whether a mutating command should preview its plan or actually write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DryRun {
    Yes,
    No,
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Locate the store for `cwd`, verify its format version, and clear what an
/// interrupted write left, if no other process holds the lock. Every command
/// funnels through here so a prior crash heals on the next invocation.
pub(crate) fn discover_store(cwd: &Path) -> Result<Store> {
    let store = Store::discover(cwd)?;
    store.check_version()?;
    store.try_recover()?;
    Ok(store)
}

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

/// Open the store for a read-only command: discover, load state, and warn on
/// any consistency issues. Waits for no lock: the load runs unlocked, and
/// recovery in [`discover_store`] is skipped while another process holds it.
pub(crate) fn open_store(cwd: &Path) -> Result<(Store, ProjectState)> {
    let store = discover_store(cwd)?;
    let state = store.load_state()?;
    warn_on_issues(&state);
    Ok((store, state))
}

/// Open the store for a mutating command: discover, acquire the exclusive file
/// lock *before* loading state (closing the TOCTOU window between load and
/// write), then load and warn on consistency issues.
pub(crate) fn open_store_mut(cwd: &Path) -> Result<(Store, store::StoreLock, ProjectState)> {
    let store = discover_store(cwd)?;
    let ((), lock, state) = store.begin_write(|| ())?;
    warn_on_issues(&state);
    Ok((store, lock, state))
}
