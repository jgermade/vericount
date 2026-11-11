//! HTTP server for vericounts: serves the web client and the API.

pub mod aeat_client;
pub mod api;
pub mod auth;
pub mod certificate;
pub mod config;
pub mod directory;
pub mod mail;
pub mod passkeys;
pub mod security;
pub mod store;
pub mod submission;

use std::{path::Path, sync::Arc};

use axum::{middleware, routing::get, Router};
use sqlx::PgPool;
use tower_http::services::ServeDir;
use webauthn_rs::Webauthn;

use crate::config::Config;

/// Database migrations, embedded in the binary.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!();

/// Shared state of the request handlers.
#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub config: Arc<Config>,
    pub mailer: Arc<mail::Mailer>,
    /// Passkey settings, derived from `config.public_url`.
    pub webauthn: Arc<Webauthn>,
    /// Present when sending records to the AEAT is enabled.
    pub submitter: Option<Arc<submission::Submitter>>,
}

/// Builds the application router.
///
/// `web_dir` is the directory with the static web client. Every response,
/// including errors and static files, goes through the security headers
/// middleware.
pub fn app(state: AppState, web_dir: impl AsRef<Path>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .nest("/api", api::router(state.clone()))
        .fallback_service(ServeDir::new(web_dir))
        .layer(middleware::from_fn(security::headers))
        .with_state(state)
}

async fn healthz() -> &'static str {
    "ok"
}

/// Makes `role`, the server's login role, a member of `vericounts_app`,
/// which the first migration creates. Returns false if it already was one;
/// then nothing is granted, so the owner only needs the right to grant it
/// the first time.
pub async fn grant_app_role(pool: &PgPool, role: &str) -> Result<bool, String> {
    let valid = role.len() <= 63
        && role.starts_with(|c: char| c.is_ascii_lowercase() || c == '_')
        && role
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if !valid {
        return Err(format!("{role:?} is not a plain role name"));
    }
    let member: Option<bool> = sqlx::query_scalar(
        "SELECT pg_has_role(oid, 'vericounts_app', 'MEMBER') FROM pg_roles WHERE rolname = $1",
    )
    .bind(role)
    .fetch_optional(pool)
    .await
    .map_err(|e| e.to_string())?;
    match member {
        None => Err(format!("role {role} does not exist")),
        Some(true) => Ok(false),
        Some(false) => {
            // Checked above to be a plain name, so it can go in the statement.
            sqlx::query(&format!("GRANT vericounts_app TO {role}"))
                .execute(pool)
                .await
                .map_err(|e| e.to_string())?;
            Ok(true)
        }
    }
}
