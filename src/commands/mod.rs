mod component;
mod decision;
mod gc;
mod init;
pub(crate) mod install;
mod map;
pub(crate) mod migrate;
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
pub use query::{check, query_file, status};
pub use serve::serve;

use std::path::Path;

use crate::Result;
use crate::store::{self, ProjectState, Store};

/// Whether a mutating command should preview its plan or actually write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DryRun {
    Yes,
    No,
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Locate the store for `cwd`, verify its format version, and clean up any
/// temp files left by an interrupted atomic write. Every command funnels
/// through here so a prior crash self-heals on the next invocation.
pub(crate) fn discover_store(cwd: &Path) -> Result<Store> {
    let store = Store::discover(cwd)?;
    store.check_version()?;
    let stale = store.clean_stale_tmp()?;
    if stale > 0 {
        eprintln!("warning: cleaned {stale} stale temp file(s) from interrupted write");
    }
    Ok(store)
}

fn warn_on_issues(state: &ProjectState) {
    let issues = state.validate();
    let errors = issues
        .iter()
        .filter(|i| i.severity == crate::store::graph::Severity::Error)
        .count();
    if errors > 0 {
        eprintln!(
            "warning: .trurlic/ has {errors} consistency issue(s) — run `trurlic check` for details"
        );
    }
}

/// Open the store for a read-only command: discover, load state, and warn on
/// any consistency issues. Acquires no lock — read commands never pay locking
/// cost (see `tiered-store-access-helpers-separate-read-only-from-mutable`).
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
    let lock = store.lock()?;
    let state = store.load_state()?;
    warn_on_issues(&state);
    Ok((store, lock, state))
}
