mod bootstrap;
mod component;
mod decision;
mod design;
mod gc;
mod init;
pub(crate) mod install;
mod map;
pub(crate) mod migrate;
mod query;
mod serve;

pub use bootstrap::{bootstrap, bootstrap_component, bootstrap_direct};
pub use component::{
    add_component, add_connection, remove_component, remove_connection, rename_component,
};
pub use decision::{decide, remove_agent_decisions, remove_decision};
pub use design::design;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DryRun {
    Yes,
    No,
}

// ── Helpers ──────────────────────────────────────────────────────────────────

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

pub(crate) fn open_store(cwd: &Path) -> Result<(Store, ProjectState)> {
    let store = discover_store(cwd)?;
    let state = store.load_state()?;
    warn_on_issues(&state);
    Ok((store, state))
}

pub(crate) fn open_store_mut(cwd: &Path) -> Result<(Store, store::StoreLock, ProjectState)> {
    let store = discover_store(cwd)?;
    let lock = store.lock()?;
    let state = store.load_state()?;
    warn_on_issues(&state);
    Ok((store, lock, state))
}
