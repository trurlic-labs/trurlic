//! Interactive map server: axum HTTP + WebSocket on `127.0.0.1`.
//!
//! `trurlic map` starts a token-gated local server that serves the graph
//! visualization and a REST API for mutations. A file watcher detects
//! external changes (MCP writes, CLI, git) and pushes diffs over
//! WebSocket. See `trurlic-map-spec.md` for the full architecture.

pub(crate) mod api;
pub(crate) mod diff;
pub(crate) mod embed;
pub(crate) mod layout;
pub(crate) mod token;
pub(crate) mod ws;

use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::Duration;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::http::header::{
    CONTENT_SECURITY_POLICY, HeaderValue, X_CONTENT_TYPE_OPTIONS, X_FRAME_OPTIONS,
};
use axum::middleware;
use axum::routing::{delete, get, post, put};
use tokio::sync::broadcast;
use tower_http::cors::CorsLayer;

use crate::console::diag;
use crate::store::watcher::WatcherGuard;
use crate::store::{ProjectState, Store, StoreLock};

use layout::LayoutState;

// ── Broadcast channel capacity ─────────────────────────────────────────────

/// Buffer size for the WebSocket broadcast channel. Events beyond this
/// count cause lagging receivers to get a `Lagged` error, which the
/// WebSocket handler converts to a `full_reload`. 256 events covers
/// typical CLI/MCP bursts with headroom.
const WS_BROADCAST_CAPACITY: usize = 256;

// ── Shared state ───────────────────────────────────────────────────────────

pub(crate) struct MapState {
    pub store: Store,
    project_state: RwLock<ProjectState>,
    layout: RwLock<LayoutState>,
    pub token: String,
    pub ws_tx: broadcast::Sender<Arc<str>>,
}

impl MapState {
    pub(crate) fn read_project_state(&self) -> RwLockReadGuard<'_, ProjectState> {
        self.project_state.read().unwrap_or_else(|p| p.into_inner())
    }

    pub(crate) fn write_project_state(&self) -> RwLockWriteGuard<'_, ProjectState> {
        self.project_state
            .write()
            .unwrap_or_else(|p| p.into_inner())
    }

    /// Take the state write lock, then the file lock, and reload the graph
    /// from disk, so the write validates against what other processes
    /// committed. Clients get the diff of that reload first, under the same
    /// lock as the watcher's diffs, so every event arrives in commit order.
    pub(crate) fn begin_write(
        &self,
    ) -> crate::Result<(RwLockWriteGuard<'_, ProjectState>, StoreLock)> {
        let (mut current, lock, loaded) = self.store.begin_write(|| self.write_project_state())?;
        ws::broadcast(&self.ws_tx, &diff::diff_states(&current, &loaded));
        *current = loaded;
        Ok((current, lock))
    }

    pub(crate) fn read_layout(&self) -> RwLockReadGuard<'_, LayoutState> {
        self.layout.read().unwrap_or_else(|p| p.into_inner())
    }

    pub(crate) fn write_layout(&self) -> RwLockWriteGuard<'_, LayoutState> {
        self.layout.write().unwrap_or_else(|p| p.into_inner())
    }
}

// ── Public entry point ─────────────────────────────────────────────────────

pub(crate) async fn start(
    store: Store,
    state: ProjectState,
    port: Option<u16>,
    no_open: bool,
) -> crate::Result<()> {
    let token = token::generate();
    let layout = layout::load(store.root());
    let (ws_tx, _) = broadcast::channel::<Arc<str>>(WS_BROADCAST_CAPACITY);

    let map_state = Arc::new(MapState {
        store,
        project_state: RwLock::new(state),
        layout: RwLock::new(layout),
        token: token.clone(),
        ws_tx: ws_tx.clone(),
    });

    // Build router.
    let bearer: Arc<str> = Arc::from(token.as_str());

    let api_routes = Router::new()
        .route("/graph", get(api::get_graph))
        .route("/layout", put(api::put_layout))
        .route("/layout/reset", post(api::reset_layout))
        .route("/component", post(api::post_component))
        .route("/component/:name", delete(api::delete_component))
        .route("/connection", post(api::post_connection))
        .route("/connection/:from/:to", delete(api::delete_connection))
        .route(
            "/decision/:name",
            put(api::put_decision).delete(api::delete_decision),
        )
        .route_layer(middleware::from_fn_with_state(
            bearer,
            token::require_bearer,
        ))
        .with_state(map_state.clone());

    // Bind to 127.0.0.1 only — never 0.0.0.0.
    // Bind before building the router so the CSP can reference the actual port.
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port.unwrap_or(0)));
    let listener = TcpListener::bind(addr).map_err(|e| {
        crate::Error::Io(std::io::Error::new(
            e.kind(),
            format!("failed to bind {addr}: {e}"),
        ))
    })?;
    let local_addr = listener.local_addr().map_err(crate::Error::Io)?;
    listener.set_nonblocking(true).map_err(crate::Error::Io)?;
    let listener = tokio::net::TcpListener::from_std(listener).map_err(crate::Error::Io)?;

    // Build CSP with the actual bound port — no wildcard.
    let csp = format!(
        "default-src 'self'; \
         script-src 'self'; \
         style-src 'self' 'unsafe-inline'; \
         connect-src 'self' ws://127.0.0.1:{}",
        local_addr.port(),
    );

    let csp_header = HeaderValue::try_from(csp)
        .map_err(|e| crate::Error::Validation(format!("invalid CSP header value: {e}")))?;

    let app = Router::new()
        .nest("/api", api_routes)
        .route("/ws", get(ws::handler))
        .fallback(embed::static_handler)
        .with_state(map_state.clone())
        .layer(
            tower_http::set_header::SetResponseHeaderLayer::if_not_present(
                CONTENT_SECURITY_POLICY,
                csp_header,
            ),
        )
        .layer(
            tower_http::set_header::SetResponseHeaderLayer::if_not_present(
                X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            ),
        )
        .layer(
            tower_http::set_header::SetResponseHeaderLayer::if_not_present(
                X_FRAME_OPTIONS,
                HeaderValue::from_static("DENY"),
            ),
        )
        .layer(DefaultBodyLimit::max(1_048_576)) // 1 MB
        .layer(CorsLayer::new()); // Deny all cross-origin requests (spec: §Security).

    let url = format!("http://{local_addr}/?token={token}");
    diag!("trurlic: map \u{2192} {url}");

    // Start file watcher.
    let _watcher_guard = spawn_watcher(map_state.clone());

    // Open browser.
    if !no_open && let Err(e) = opener::open(&url) {
        diag!("trurlic: failed to open browser: {e}");
    }

    // Run until Ctrl+C.
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(crate::Error::Io)?;

    diag!("trurlic: map server stopped");
    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    diag!("\ntrurlic: shutting down...");
}

// ── File watcher ───────────────────────────────────────────────────────────

/// Debounce window for the map watcher. Lower than the MCP watcher
/// (100ms) — the interactive UI benefits from faster updates at the
/// cost of slightly more reloads during multi-file operations.
const MAP_DEBOUNCE: Duration = Duration::from_millis(50);

/// Spawn a file watcher that detects external `.trurlic/` changes,
/// diffs the state, and pushes events over the WebSocket broadcast
/// channel.
fn spawn_watcher(state: Arc<MapState>) -> Option<WatcherGuard> {
    let store_root = state.store.root().to_path_buf();

    let served = Arc::clone(&state);
    match crate::store::watcher::spawn(
        &store_root,
        MAP_DEBOUNCE,
        "trurlic-map-watcher",
        move || served.read_project_state().generation(),
        move |loaded, served_at_load| {
            // Diff and swap under one write lock: an API write between the
            // two would otherwise be overwritten, and its events reordered.
            let mut current = state.write_project_state();
            if loaded.is_overtaken(&current, served_at_load) {
                return;
            }
            ws::broadcast(&state.ws_tx, &diff::diff_states(&current, &loaded));
            *current = loaded;
        },
    ) {
        Ok(guard) => {
            diag!("trurlic: file watcher active");
            Some(guard)
        }
        Err(e) => {
            diag!("trurlic: file watcher unavailable: {e}");
            None
        }
    }
}
