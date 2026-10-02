//! MCP server file watcher — delegates to the shared store watcher
//! with MCP-specific state swap and validation logging.
//!
//! The MCP watcher uses a 100ms debounce window. This is higher than
//! the map watcher (50ms) because the MCP server has no interactive UI
//! and benefits more from coalescing rapid changes (e.g. `git checkout`
//! touching many files) into a single reload.

use std::sync::{Arc, PoisonError, RwLock};
use std::time::Duration;

use crate::store::ProjectState;
use crate::store::graph::Severity;

pub(crate) use crate::store::watcher::WatcherGuard;

/// Debounce window for the MCP server watcher. Higher than the map
/// watcher (50ms) — the MCP server has no interactive UI, so
/// coalescing multi-file operations outweighs latency.
const DEBOUNCE: Duration = Duration::from_millis(100);

/// Spawn a file watcher that reloads `.trurlic/` state into the shared
/// `Arc<RwLock<ProjectState>>` on external changes.
///
/// Returns a guard whose lifetime controls the watcher. Failure is
/// non-fatal — the caller logs the error and continues without live
/// reload.
pub(crate) fn spawn(
    store_root: &std::path::Path,
    state: Arc<RwLock<ProjectState>>,
) -> Result<WatcherGuard, String> {
    let served = Arc::clone(&state);
    let served_generation = move || {
        served
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .generation()
    };
    crate::store::watcher::spawn(
        store_root,
        DEBOUNCE,
        "trurlic-watcher",
        served_generation,
        move |loaded, served_at_load| {
            let errors = loaded
                .validate()
                .iter()
                .filter(|i| i.severity == Severity::Error)
                .count();

            let mut current = state.write().unwrap_or_else(|poisoned| {
                eprintln!("trurlic: recovered from poisoned state lock");
                poisoned.into_inner()
            });
            if loaded.is_overtaken(&current, served_at_load) {
                return;
            }
            *current = loaded;
            drop(current);

            if errors > 0 {
                eprintln!("trurlic: reloaded state ({errors} consistency issue(s))");
            }
        },
    )
}
