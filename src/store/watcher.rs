//! File watcher for live reload of `.trurlic/` changes.
//!
//! `trurlic serve` reloads the graph when another process changes
//! `.trurlic/` (CLI writes, manual edits, git checkout) and swaps it into the
//! state it serves: watch -> filter -> debounce -> reload -> swap.
//!
//! The reload holds the shared file lock, so it never reads a commit
//! halfway through its renames, and releases it before the swap takes the
//! state write lock: writers take that lock before the file lock, so no
//! thread may hold a file lock while waiting for it. Events that arrive
//! during a reload stay queued and start the next one: they may stem from a
//! commit that landed after this load.
//!
//! Events inside `.state/` (tmp files, lock, generation) are ignored:
//! they never change the graph.

use std::path::Path;
use std::sync::{Arc, PoisonError, RwLock, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};

use super::graph::Severity;
use super::{STATE_DIR, Store};
use crate::console::diag;
use crate::store::ProjectState;

/// How long events are batched before a reload: long enough to coalesce a
/// multi-file change (a `git checkout`, one commit's renames) into one load.
const DEBOUNCE: Duration = Duration::from_millis(100);

// ── Guard ────────────────────────────────────────────────────────────────────

/// Handle that keeps the watcher alive. Dropping stops the watch.
///
/// When dropped, the internal [`RecommendedWatcher`] is dropped, which
/// destroys the event callback and closes the channel sender. The
/// watcher thread sees `Disconnected` on its next `recv` and exits.
pub(crate) struct WatcherGuard {
    _watcher: RecommendedWatcher,
}

// ── Public API ──────────────────────────────────────────────────────────────

/// Spawn a background thread that watches `.trurlic/` and swaps each
/// freshly loaded [`ProjectState`] into `state`, unless the load
/// [`is_overtaken`](ProjectState::is_overtaken) by a write of this server.
///
/// Failure to create the watcher is non-fatal: the caller logs the error
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

    thread::Builder::new()
        .name("trurlic-watcher".into())
        .spawn(move || watch_loop(&store, &rx, &state))
        .map_err(|e| format!("failed to spawn watcher thread: {e}"))?;

    Ok(WatcherGuard { _watcher: watcher })
}

// ── Internals ──────────────────────────────────────────────────────────────

/// Event loop: block -> filter -> debounce -> reload -> swap -> repeat.
/// Returns when the channel closes, i.e. when the guard is dropped.
fn watch_loop(store: &Store, rx: &mpsc::Receiver<notify::Event>, state: &RwLock<ProjectState>) {
    let state_dir = store.root().join(STATE_DIR);
    while let Ok(event) = rx.recv() {
        if !is_relevant(&event, &state_dir) {
            continue;
        }
        debounce_events(rx);

        // A failed reload keeps the served state; the write or edit that
        // completes the store sends the events for the next attempt.
        let served_at_load = state
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .generation();
        match store.load_shared() {
            Ok(loaded) => swap(state, loaded, served_at_load),
            Err(e) => diag!(
                "trurlic: watcher reload of {} failed: {e}",
                store.root().display()
            ),
        }
    }
}

/// Swap `loaded` in under the state write lock, taken with no file lock
/// held, unless a write of this server overtook the load.
fn swap(state: &RwLock<ProjectState>, loaded: ProjectState, served_at_load: u64) {
    let errors = loaded
        .validate()
        .iter()
        .filter(|i| i.severity() == Severity::Error)
        .count();

    let mut current = state.write().unwrap_or_else(|poisoned| {
        diag!("trurlic: recovered from poisoned state lock");
        poisoned.into_inner()
    });
    if loaded.is_overtaken(&current, served_at_load) {
        return;
    }
    *current = loaded;
    drop(current);

    if errors > 0 {
        diag!("trurlic: reloaded state ({errors} consistency issue(s))");
    }
}

/// Returns `true` if any event path is outside `.state/`.
fn is_relevant(event: &notify::Event, state_dir: &Path) -> bool {
    event.paths.iter().any(|p| !p.starts_with(state_dir))
}

/// Wait for the [`DEBOUNCE`] window, consuming all events that arrive.
fn debounce_events(rx: &mpsc::Receiver<notify::Event>) {
    let deadline = Instant::now() + DEBOUNCE;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return;
        }
        match rx.recv_timeout(remaining) {
            Ok(_) => continue,
            Err(mpsc::RecvTimeoutError::Timeout) => return,
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn event_at(paths: &[&str]) -> notify::Event {
        let mut e = notify::Event::new(notify::EventKind::Any);
        e.paths = paths.iter().map(PathBuf::from).collect();
        e
    }

    // ── is_relevant ─────────────────────────────────────────────────────

    #[test]
    fn relevant_for_component_file() {
        let sd = PathBuf::from("/repo/.trurlic/.state");
        assert!(is_relevant(
            &event_at(&["/repo/.trurlic/components/auth.toml"]),
            &sd,
        ));
    }

    #[test]
    fn relevant_for_graph_toml() {
        let sd = PathBuf::from("/repo/.trurlic/.state");
        assert!(is_relevant(&event_at(&["/repo/.trurlic/graph.toml"]), &sd));
    }

    #[test]
    fn relevant_for_project_toml() {
        let sd = PathBuf::from("/repo/.trurlic/.state");
        assert!(is_relevant(
            &event_at(&["/repo/.trurlic/project.toml"]),
            &sd
        ));
    }

    #[test]
    fn irrelevant_for_lock_file() {
        let sd = PathBuf::from("/repo/.trurlic/.state");
        assert!(!is_relevant(
            &event_at(&["/repo/.trurlic/.state/lock"]),
            &sd
        ));
    }

    #[test]
    fn irrelevant_for_tmp_file() {
        let sd = PathBuf::from("/repo/.trurlic/.state");
        assert!(!is_relevant(
            &event_at(&["/repo/.trurlic/.state/tmp/0_auth.toml"]),
            &sd,
        ));
    }

    #[test]
    fn irrelevant_for_session_file() {
        let sd = PathBuf::from("/repo/.trurlic/.state");
        assert!(!is_relevant(
            &event_at(&["/repo/.trurlic/.state/sessions/auth.json"]),
            &sd,
        ));
    }

    #[test]
    fn relevant_if_any_path_outside_state() {
        let sd = PathBuf::from("/repo/.trurlic/.state");
        assert!(is_relevant(
            &event_at(&[
                "/repo/.trurlic/.state/lock",
                "/repo/.trurlic/decisions/use-jwt.toml",
            ]),
            &sd,
        ));
    }

    #[test]
    fn irrelevant_for_empty_paths() {
        let sd = PathBuf::from("/repo/.trurlic/.state");
        assert!(!is_relevant(&event_at(&[]), &sd));
    }
}
