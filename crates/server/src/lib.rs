//! HTTP server for vericounts: serves the web client and the API.

pub mod api;
pub mod config;
pub mod security;
pub mod store;

use std::{path::Path, sync::Arc};

use axum::{middleware, routing::get, Router};
use sqlx::PgPool;
use tower_http::services::ServeDir;

use crate::config::Config;

/// Database migrations, embedded in the binary.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!();

/// Shared state of the request handlers.
#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub config: Arc<Config>,
}

/// Builds the application router.
///
/// `web_dir` is the directory with the static web client. Every response,
/// including errors and static files, goes through the security headers
/// middleware.
pub fn app(state: AppState, web_dir: impl AsRef<Path>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .nest("/api", api::router())
        .fallback_service(ServeDir::new(web_dir))
        .layer(middleware::from_fn(security::headers))
        .with_state(state)
}

async fn healthz() -> &'static str {
    "ok"
}
