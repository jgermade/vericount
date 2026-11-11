//! HTTP server for vericounts: serves the web client and the API.

pub mod security;

use std::path::Path;

use axum::{middleware, routing::get, Router};
use tower_http::services::ServeDir;

/// Builds the application router.
///
/// `web_dir` is the directory with the static web client. Every response,
/// including errors and static files, goes through the security headers
/// middleware.
pub fn app(web_dir: impl AsRef<Path>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .fallback_service(ServeDir::new(web_dir))
        .layer(middleware::from_fn(security::headers))
}

async fn healthz() -> &'static str {
    "ok"
}
