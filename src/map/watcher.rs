//! File watcher for `.trurl/` live updates.
//!
//! Watches the store root recursively so that atomic renames of
//! `project.toml` (write-to-tmp then rename) are correctly detected on
//! all platforms.  Events inside `.state/` (lock, sessions, temp files)
//! are filtered out — only content-bearing changes trigger a broadcast.

use std::path::Path;

use notify::{RecursiveMode, Watcher};
use tokio::sync::broadcast;

use crate::store::schema::STATE_DIR;

/// Start watching `.trurl/` for content changes.
///
/// Returns the watcher handle — dropping it stops watching.
/// The caller must keep it alive for the server's lifetime.
pub(super) fn start(
    store_root: &Path,
    tx: broadcast::Sender<()>,
) -> notify::Result<notify::RecommendedWatcher> {
    let state_dir = store_root.join(STATE_DIR);

    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(event) = res {
            // Only signal for content-bearing changes — ignore .state/
            // (lock file, sessions, temp files from atomic writes).
            let relevant = event.paths.iter().any(|p| !p.starts_with(&state_dir));
            if relevant {
                let _ = tx.send(());
            }
        }
    })?;

    watcher.watch(store_root, RecursiveMode::Recursive)?;

    Ok(watcher)
}
