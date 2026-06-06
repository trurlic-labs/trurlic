//! WebSocket handler for live map updates.
//!
//! Each connected browser receives the full project state on connect
//! and again whenever `.trurl/` changes on disk.  Changes are debounced
//! (100 ms) to coalesce rapid file-system events from atomic writes.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket};
use tokio::sync::broadcast;

use crate::store::Store;

/// Drive a single WebSocket connection.
pub(super) async fn handle(
    mut socket: WebSocket,
    store_root: Arc<Path>,
    mut rx: broadcast::Receiver<()>,
) {
    // Send initial state immediately.
    if let Some(json) = state_json(&store_root) {
        if socket.send(Message::Text(json)).await.is_err() {
            return;
        }
    }

    loop {
        tokio::select! {
            result = rx.recv() => {
                match result {
                    Ok(()) | Err(broadcast::error::RecvError::Lagged(_)) => {
                        // Debounce: wait, drain, then push.
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        while rx.try_recv().is_ok() {}

                        if let Some(json) = state_json(&store_root) {
                            if socket.send(Message::Text(json)).await.is_err() {
                                break;
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Ping(data))) => {
                        if socket.send(Message::Pong(data)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {}
                }
            }
        }
    }
}

/// Load `.trurl/` and serialize to a JSON string for the frontend.
fn state_json(store_root: &Path) -> Option<String> {
    let store = Store::at(store_root.to_path_buf());
    let state = store.load_state().ok()?;
    serde_json::to_string(&super::serialize_state(&state)).ok()
}
