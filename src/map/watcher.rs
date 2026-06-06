//! File watcher for `.trurl/` live updates.
//!
//! Watches `components/` and `decisions/` directories plus `project.toml`.
//! On any change, fires a unit signal through the broadcast channel.
//! Consumers (WebSocket handlers) debounce and reload state.

use std::path::Path;

use notify::{RecursiveMode, Watcher};
use tokio::sync::broadcast;

use crate::store::schema::{COMPONENTS_DIR, DECISIONS_DIR};

/// Start watching `.trurl/` for content changes.
///
/// Returns the watcher handle — dropping it stops watching.
/// The caller must keep it alive for the server's lifetime.
pub(super) fn start(
    store_root: &Path,
    tx: broadcast::Sender<()>,
) -> notify::Result<notify::RecommendedWatcher> {
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if res.is_ok() {
            let _ = tx.send(());
        }
    })?;

    let components = store_root.join(COMPONENTS_DIR);
    let decisions = store_root.join(DECISIONS_DIR);
    let project = store_root.join("project.toml");

    if components.is_dir() {
        watcher.watch(&components, RecursiveMode::NonRecursive)?;
    }
    if decisions.is_dir() {
        watcher.watch(&decisions, RecursiveMode::NonRecursive)?;
    }
    if project.is_file() {
        watcher.watch(&project, RecursiveMode::NonRecursive)?;
    }

    Ok(watcher)
}
