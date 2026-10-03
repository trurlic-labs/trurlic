//! WebSocket handler for live graph updates.
//!
//! Single server→client push channel. All mutations go through the REST
//! API; the WebSocket carries only diff events. The token is validated
//! from the `?token=` query parameter (browser WebSocket APIs cannot set
//! custom headers).

use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde::Deserialize;
use tokio::sync::broadcast;

use super::MapState;
use super::diff::WsEvent;
use crate::console::diag;

// ── Types ──────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub(crate) struct WsQuery {
    token: Option<String>,
}

// ── Handler ────────────────────────────────────────────────────────────────

/// WebSocket upgrade handler. Validates the token from the query string,
/// then upgrades the connection and subscribes to the broadcast channel.
pub(crate) async fn handler(
    State(state): State<Arc<MapState>>,
    Query(query): Query<WsQuery>,
    ws: WebSocketUpgrade,
) -> Result<Response, StatusCode> {
    let valid = query
        .token
        .as_deref()
        .is_some_and(|t| super::token::constant_time_eq(t.as_bytes(), state.token.as_bytes()));

    if !valid {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let rx = state.ws_tx.subscribe();
    Ok(ws.on_upgrade(move |socket| handle_socket(socket, rx)))
}

/// Per-connection task: forward broadcast events as JSON text frames.
/// Ignores client→server messages (the WebSocket is one-way push).
/// Exits when the client disconnects or the broadcast channel closes.
async fn handle_socket(mut socket: WebSocket, mut rx: broadcast::Receiver<Arc<str>>) {
    loop {
        tokio::select! {
            // Forward server events to the client.
            msg = rx.recv() => {
                match msg {
                    Ok(json) => {
                        if socket.send(Message::Text(json.to_string())).await.is_err() {
                            return; // client disconnected
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        // Client fell behind — send full_reload so it re-syncs.
                        diag!("trurlic: ws client lagged {n} events, sending full_reload");
                        let reload = r#"{"type":"full_reload"}"#;
                        if socket.send(Message::Text(reload.into())).await.is_err() {
                            return;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
            // Drain client→server messages (ignored, but must be read to
            // detect disconnects and prevent backpressure).
            msg = socket.recv() => {
                match msg {
                    Some(Ok(_)) => {} // ignore client messages
                    _ => return,      // disconnected or error
                }
            }
        }
    }
}

/// Serialize and broadcast a batch of events. Each event is sent as a
/// separate text frame so the client can process them incrementally.
pub(crate) fn broadcast(tx: &broadcast::Sender<Arc<str>>, events: &[WsEvent]) {
    for event in events {
        if let Ok(json) = serde_json::to_string(event) {
            // Receiver count of 0 is fine — no connected clients.
            let _ = tx.send(Arc::from(json.as_str()));
        }
    }
}
