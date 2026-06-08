//! Embedded frontend assets served via `rust-embed`.
//!
//! The TypeScript frontend is compiled to `dist/` at release time by
//! esbuild. `rust-embed` includes those files in the binary at compile
//! time — users see zero build step. `static_handler` serves them as
//! the axum fallback route for non-API, non-WS paths.

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use rust_embed::Embed;

#[derive(Embed)]
#[folder = "src/map/frontend/dist/"]
struct Assets;

/// Fallback handler: serve embedded static files. Returns `index.html`
/// for paths without a file extension (SPA routing support).
pub(crate) async fn static_handler(uri: axum::http::Uri) -> impl IntoResponse {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() || !path.contains('.') {
        "index.html"
    } else {
        path
    };

    match Assets::get(path) {
        Some(content) => {
            let body = match content.data {
                std::borrow::Cow::Borrowed(bytes) => axum::body::Body::from(bytes),
                std::borrow::Cow::Owned(vec) => axum::body::Body::from(vec),
            };
            Response::builder()
                .header(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static(mime_for(path)),
                )
                .body(body)
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

fn mime_for(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "application/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}
