//! Interactive map server: axum HTTP + WebSocket on `127.0.0.1`.
//!
//! `trurl map` starts a token-gated local server that serves the graph
//! visualization and a REST API for mutations. A file watcher detects
//! external changes (MCP writes, CLI, git) and pushes diffs over
//! WebSocket. See `trurl-map-spec.md` for the full architecture.

pub(crate) mod api;
pub(crate) mod diff;
pub(crate) mod embed;
pub(crate) mod layout;
pub(crate) mod token;
pub(crate) mod ws;

use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::path::Path;
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::thread;
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::http::header::{
    CONTENT_SECURITY_POLICY, HeaderValue, X_CONTENT_TYPE_OPTIONS, X_FRAME_OPTIONS,
};
use axum::middleware;
use axum::routing::{delete, get, post, put};
use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::broadcast;
use tower_http::cors::CorsLayer;

use crate::store::{ProjectState, STATE_DIR, Store};

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

    let app = Router::new()
        .nest("/api", api_routes)
        .route("/ws", get(ws::handler))
        .fallback(embed::static_handler)
        .with_state(map_state.clone())
        .layer(
            tower_http::set_header::SetResponseHeaderLayer::if_not_present(
                CONTENT_SECURITY_POLICY,
                HeaderValue::from_static(
                    "default-src 'self'; \
                     script-src 'self'; \
                     style-src 'self' 'unsafe-inline'; \
                     connect-src 'self' ws://127.0.0.1:*; \
                     img-src 'self' data:; \
                     font-src 'none'; \
                     object-src 'none'; \
                     base-uri 'none'; \
                     form-action 'none'",
                ),
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

    // Bind to 127.0.0.1 only — never 0.0.0.0.
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port.unwrap_or(0)));
    let listener = TcpListener::bind(addr).map_err(|e| {
        crate::Error::Io(std::io::Error::new(
            e.kind(),
            format!("failed to bind {addr}: {e}"),
        ))
    })?;
    let local_addr = listener.local_addr().map_err(crate::Error::Io)?;
    let listener = tokio::net::TcpListener::from_std(listener).map_err(crate::Error::Io)?;

    let url = format!("http://{local_addr}/?token={token}");
    eprintln!("trurl: map → {url}");

    // Start file watcher.
    let _watcher_guard = spawn_watcher(map_state.clone());

    // Open browser.
    if !no_open && let Err(e) = opener::open(&url) {
        eprintln!("trurl: failed to open browser: {e}");
    }

    // Run until Ctrl+C.
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(crate::Error::Io)?;

    eprintln!("trurl: map server stopped");
    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    eprintln!("\ntrurl: shutting down...");
}

// ── File watcher ───────────────────────────────────────────────────────────

const DEBOUNCE: Duration = Duration::from_millis(50);

/// Spawn a file watcher that detects external `.trurl/` changes and
/// pushes diffs over the WebSocket broadcast channel.
fn spawn_watcher(state: Arc<MapState>) -> Option<RecommendedWatcher> {
    let store_root = state.store.root().to_path_buf();
    let state_dir = store_root.join(STATE_DIR);

    let (tx, rx) = std::sync::mpsc::channel();

    let mut watcher = match RecommendedWatcher::new(
        move |result: Result<notify::Event, notify::Error>| {
            if let Ok(event) = result {
                let _ = tx.send(event);
            }
        },
        Config::default(),
    ) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("trurl: file watcher unavailable: {e}");
            return None;
        }
    };

    if let Err(e) = watcher.watch(&store_root, RecursiveMode::Recursive) {
        eprintln!("trurl: failed to watch {}: {e}", store_root.display());
        return None;
    }

    let watcher_store = Store::at(store_root);

    if let Err(e) = thread::Builder::new()
        .name("trurl-map-watcher".into())
        .spawn(move || watcher_loop(&watcher_store, &state, &state_dir, rx))
    {
        eprintln!("trurl: failed to spawn watcher thread: {e}");
    }

    eprintln!("trurl: file watcher active");
    Some(watcher)
}

fn watcher_loop(
    store: &Store,
    state: &Arc<MapState>,
    state_dir: &Path,
    rx: std::sync::mpsc::Receiver<notify::Event>,
) {
    loop {
        let event = match rx.recv() {
            Ok(e) => e,
            Err(_) => return,
        };

        if event.paths.iter().all(|p| p.starts_with(state_dir)) {
            continue;
        }

        // Debounce.
        let deadline = Instant::now() + DEBOUNCE;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match rx.recv_timeout(remaining) {
                Ok(_) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }

        // Snapshot old state, reload, diff, broadcast.
        match store.load_state() {
            Ok(new_state) => {
                let events = {
                    let old = state.read_project_state();
                    diff::diff_states(&old, &new_state)
                };

                if !events.is_empty() {
                    ws::broadcast(&state.ws_tx, &events);
                }

                let mut guard = state.write_project_state();
                *guard = new_state;
            }
            Err(e) => {
                eprintln!("trurl: watcher reload failed: {e}");
            }
        }

        // Drain events that arrived during reload.
        while rx.try_recv().is_ok() {}
    }
}
