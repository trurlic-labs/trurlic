//! File system watcher for live reload of `.trurl/` changes.
//!
//! Watches the store directory recursively. External changes (CLI writes,
//! manual edits, git checkout) trigger a full state reload after a 100ms
//! debounce window. The reload builds a new [`ProjectState`] from disk
//! with no locks held, then swaps it into the shared `Arc<RwLock<_>>`
//! with the write lock held only for the pointer swap (microseconds).
//!
//! Events inside `.state/` (tmp files, lock, sessions) are ignored —
//! they are transient and never affect the graph.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};

use crate::store::{ProjectState, STATE_DIR, Store};

/// Debounce window: collect events for this long before reloading.
/// Batches rapid changes (e.g. `git checkout` switching many files).
const DEBOUNCE: Duration = Duration::from_millis(100);

// ── Guard ────────────────────────────────────────────────────────────────────

/// Handle that keeps the watcher alive. Dropping stops the watch.
///
/// When dropped, the internal `RecommendedWatcher` is dropped, which
/// destroys the event callback and closes the channel sender. The
/// watcher thread sees `Disconnected` on its next `recv` and exits.
pub(crate) struct WatcherGuard {
    _watcher: RecommendedWatcher,
}

// ── Public API ──────────────────────────────────────────────────────────────

/// Spawn a background thread that watches `.trurl/` and reloads state
/// on external changes. Returns a guard whose lifetime controls the watcher.
///
/// Failure to create the watcher is non-fatal — the caller logs the error
/// and continues without live reload.
pub(crate) fn spawn(
    store_root: &Path,
    state: Arc<RwLock<ProjectState>>,
) -> Result<WatcherGuard, String> {
    let (tx, rx) = mpsc::channel();

    let mut watcher = RecommendedWatcher::new(
        move |result: Result<notify::Event, notify::Error>| {
            if let Ok(event) = result {
                let _ = tx.send(event);
            }
        },
        Config::default(),
    )
    .map_err(|e| format!("failed to create file watcher: {e}"))?;

    watcher
        .watch(store_root, RecursiveMode::Recursive)
        .map_err(|e| format!("failed to watch {}: {e}", store_root.display()))?;

    let store = Store::at(store_root.to_path_buf());
    let state_dir = store_root.join(STATE_DIR);

    thread::Builder::new()
        .name("trurl-watcher".into())
        .spawn(move || watcher_loop(&store, &state, &state_dir, rx))
        .map_err(|e| format!("failed to spawn watcher thread: {e}"))?;

    Ok(WatcherGuard { _watcher: watcher })
}

// ── Internals ──────────────────────────────────────────────────────────────

/// Event loop: block → filter → debounce → reload → repeat.
fn watcher_loop(
    store: &Store,
    state: &Arc<RwLock<ProjectState>>,
    state_dir: &PathBuf,
    rx: mpsc::Receiver<notify::Event>,
) {
    loop {
        // Block until an event arrives.
        let event = match rx.recv() {
            Ok(e) => e,
            Err(_) => return, // channel closed — server shutting down
        };

        // Skip events inside .state/ (tmp files, lock, sessions).
        if !is_relevant(&event, state_dir) {
            continue;
        }

        // Debounce: drain all events that arrive within the window.
        let deadline = Instant::now() + DEBOUNCE;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match rx.recv_timeout(remaining) {
                Ok(_) => continue,
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }

        // Full reload: parse all files with no locks held, then swap.
        reload(store, state);
    }
}

/// Build a fresh [`ProjectState`] from disk and swap it in.
/// The write lock is held only for the assignment (microseconds).
fn reload(store: &Store, state: &Arc<RwLock<ProjectState>>) {
    match store.load_state() {
        Ok(new_state) => {
            let mut guard = state.write().unwrap_or_else(|e| e.into_inner());
            *guard = new_state;
            eprintln!("trurl: reloaded state from disk");
        }
        Err(e) => {
            eprintln!("trurl: watcher reload failed: {e}");
        }
    }
}

/// Returns `true` if any event path is outside `.state/`.
fn is_relevant(event: &notify::Event, state_dir: &Path) -> bool {
    event.paths.iter().any(|p| !p.starts_with(state_dir))
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn event_with_paths(paths: Vec<PathBuf>) -> notify::Event {
        notify::Event {
            kind: notify::EventKind::Modify(notify::event::ModifyKind::Data(
                notify::event::DataChange::Any,
            )),
            paths,
            attrs: Default::default(),
        }
    }

    #[test]
    fn relevant_for_component_file() {
        let state_dir = PathBuf::from("/repo/.trurl/.state");
        let event = event_with_paths(vec![PathBuf::from("/repo/.trurl/components/auth.toml")]);
        assert!(is_relevant(&event, &state_dir));
    }

    #[test]
    fn relevant_for_graph_toml() {
        let state_dir = PathBuf::from("/repo/.trurl/.state");
        let event = event_with_paths(vec![PathBuf::from("/repo/.trurl/graph.toml")]);
        assert!(is_relevant(&event, &state_dir));
    }

    #[test]
    fn irrelevant_for_lock_file() {
        let state_dir = PathBuf::from("/repo/.trurl/.state");
        let event = event_with_paths(vec![PathBuf::from("/repo/.trurl/.state/lock")]);
        assert!(!is_relevant(&event, &state_dir));
    }

    #[test]
    fn irrelevant_for_tmp_file() {
        let state_dir = PathBuf::from("/repo/.trurl/.state");
        let event = event_with_paths(vec![PathBuf::from("/repo/.trurl/.state/tmp/0_auth.toml")]);
        assert!(!is_relevant(&event, &state_dir));
    }

    #[test]
    fn relevant_if_mixed_paths() {
        let state_dir = PathBuf::from("/repo/.trurl/.state");
        let event = event_with_paths(vec![
            PathBuf::from("/repo/.trurl/.state/lock"),
            PathBuf::from("/repo/.trurl/decisions/use-jwt.toml"),
        ]);
        assert!(is_relevant(&event, &state_dir));
    }

    #[test]
    fn irrelevant_for_empty_paths() {
        let state_dir = PathBuf::from("/repo/.trurl/.state");
        let event = event_with_paths(vec![]);
        assert!(!is_relevant(&event, &state_dir));
    }
}
