//! Shared file watcher for live reload of `.trurlic/` changes.
//!
//! Both the MCP server and the map server need to detect external
//! changes to `.trurlic/` (CLI writes, manual edits, git checkout) and
//! reload state from disk. This module runs the shared
//! watch -> filter -> debounce -> reload loop; consumers supply a callback
//! that receives each freshly loaded [`ProjectState`].
//!
//! The reload holds the shared file lock, so it never reads a commit
//! halfway through its renames, and releases it before the callback runs:
//! the callback takes the consumer's state lock, and writers take that lock
//! before the file lock, so no thread may hold a file lock while waiting for
//! it. Events that arrive during a reload stay queued and start the next
//! one: they may stem from a commit that landed after this load.
//!
//! Events inside `.state/` (tmp files, lock, generation) are ignored:
//! they never change the graph.

use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};

use super::{STATE_DIR, Store};
use crate::store::ProjectState;

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

/// Spawn a background thread that watches `.trurlic/` and calls
/// `on_load` with a freshly loaded [`ProjectState`] whenever
/// relevant files change on disk.
///
/// `debounce` controls how long to batch events before reloading.
/// Lower values give faster UI updates; higher values coalesce
/// multi-file operations (e.g. `git checkout`).
///
/// `served_generation` reads the generation the consumer serves; it is read
/// before each load and passed to `on_load` with the loaded state. The
/// callback runs on the watcher thread with no file lock held, and swaps the
/// loaded state in unless it [`is_overtaken`](ProjectState::is_overtaken).
///
/// Failure to create the watcher is non-fatal — the caller should
/// log the error and continue without live reload.
pub(crate) fn spawn(
    store_root: &Path,
    debounce: Duration,
    thread_name: &str,
    served_generation: impl Fn() -> u64 + Send + 'static,
    on_load: impl Fn(ProjectState, u64) + Send + 'static,
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
        .name(thread_name.into())
        .spawn(move || watch_loop(&store, debounce, &rx, served_generation, on_load))
        .map_err(|e| format!("failed to spawn watcher thread: {e}"))?;

    Ok(WatcherGuard { _watcher: watcher })
}

// ── Internals ──────────────────────────────────────────────────────────────

/// Event loop: block -> filter -> debounce -> reload -> callback -> repeat.
/// Returns when the channel closes, i.e. when the guard is dropped.
fn watch_loop(
    store: &Store,
    debounce: Duration,
    rx: &mpsc::Receiver<notify::Event>,
    served_generation: impl Fn() -> u64,
    on_load: impl Fn(ProjectState, u64),
) {
    let state_dir = store.root().join(STATE_DIR);
    while let Ok(event) = rx.recv() {
        if !is_relevant(&event, &state_dir) {
            continue;
        }
        debounce_events(rx, debounce);

        // A failed reload keeps the served state; the write or edit that
        // completes the store sends the events for the next attempt.
        let served_at_load = served_generation();
        match store.load_shared() {
            Ok(loaded) => on_load(loaded, served_at_load),
            Err(e) => eprintln!(
                "trurlic: watcher reload of {} failed: {e}",
                store.root().display()
            ),
        }
    }
}

/// Returns `true` if any event path is outside `.state/`.
fn is_relevant(event: &notify::Event, state_dir: &Path) -> bool {
    event.paths.iter().any(|p| !p.starts_with(state_dir))
}

/// Wait for the debounce window, consuming all events that arrive.
fn debounce_events(rx: &mpsc::Receiver<notify::Event>, duration: Duration) {
    let deadline = Instant::now() + duration;
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
