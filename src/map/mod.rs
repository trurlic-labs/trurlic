//! Interactive architecture map — local web server with live updates.
//!
//! `start_server` binds to a random port on localhost, serves a single-page
//! frontend, exposes a REST API for reads and mutations, and pushes state
//! updates to connected browsers via WebSocket.

pub(super) mod api;
mod watcher;
mod ws;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::extract::ws::WebSocketUpgrade;
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use tokio::sync::broadcast;

use crate::Result;

// ── Shared state ──────────────────────────────────────────────────────────

pub(super) struct AppState {
    pub store_root: PathBuf,
    pub broadcast_tx: broadcast::Sender<()>,
}

// ── Server ────────────────────────────────────────────────────────────────

const INDEX_HTML: &str = include_str!("index.html");

/// Start the map server and block until Ctrl-C.
pub(crate) async fn start_server(store_root: &Path) -> Result<()> {
    let (broadcast_tx, _) = broadcast::channel::<()>(32);

    // File watcher — must outlive the server.
    let _watcher = watcher::start(store_root, broadcast_tx.clone())
        .map_err(|e| crate::Error::Validation(format!("failed to start file watcher: {e}")))?;

    let state = Arc::new(AppState {
        store_root: store_root.to_path_buf(),
        broadcast_tx,
    });

    let app = Router::new()
        .route("/", get(serve_html))
        .route("/api/state", get(api::get_state))
        .route("/api/components", post(api::add_component))
        .route("/api/components/{name}", delete(api::remove_component))
        .route("/api/connections", post(api::add_connection))
        .route(
            "/api/connections/{from}/{to}",
            delete(api::remove_connection),
        )
        .route("/ws", get(ws_upgrade))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(crate::Error::Io)?;

    let addr = listener.local_addr().map_err(crate::Error::Io)?;
    let url = format!("http://{addr}");
    eprintln!("trurl: map server at {url}");
    open_browser(&url);

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(crate::Error::Io)?;

    eprintln!("trurl: map server stopped");
    Ok(())
}

// ── Route handlers ────────────────────────────────────────────────────────

async fn serve_html() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        INDEX_HTML,
    )
}

async fn ws_upgrade(ws: WebSocketUpgrade, State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let store_root: Arc<Path> = Arc::from(state.store_root.as_path());
    let rx = state.broadcast_tx.subscribe();
    ws.on_upgrade(move |socket| ws::handle(socket, store_root, rx))
}

async fn shutdown_signal() {
    tokio::signal::ctrl_c().await.ok();
    eprintln!("\ntrurl: shutting down…");
}

// ── Browser launch ────────────────────────────────────────────────────────

fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(url).spawn();
    #[cfg(target_os = "linux")]
    let result = std::process::Command::new("xdg-open").arg(url).spawn();
    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("cmd")
        .args(["/C", "start", url])
        .spawn();
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    let result: std::result::Result<std::process::Child, std::io::Error> = Err(
        std::io::Error::new(std::io::ErrorKind::Unsupported, "unsupported platform"),
    );

    match result {
        Ok(_) => {}
        Err(e) => eprintln!("trurl: could not open browser ({e}), visit {url} manually"),
    }
}
