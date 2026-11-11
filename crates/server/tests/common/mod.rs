#![allow(dead_code)]

pub mod authenticator;

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use axum::{
    body::Body,
    http::{
        header::{CONTENT_TYPE, COOKIE},
        HeaderMap, Method, Request, StatusCode,
    },
    Router,
};
use http_body_util::BodyExt;
use serde_json::Value;
use sqlx::PgPool;
use tower::ServiceExt;
use vericount_core::qr::AeatEnvironment;
use vericount_server::{
    aeat_service::AeatService,
    config::{self, AeatSettings, Config, Issuer},
    mail::{Mail, Mailer},
    AppState,
};
use webauthn_rs::prelude::Url;

pub const ISSUER_NIF: &str = "B12345674";

/// A minimal static site, so these tests do not need the web build.
pub fn web_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/web")
}

/// The real web client: its sources and, when present, its build output.
pub fn web_client_dirs() -> Vec<PathBuf> {
    let web = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../web");
    [web.join("index.html"), web.join("dist")]
        .into_iter()
        .filter(|path| path.exists())
        .collect()
}

pub fn issuer() -> Issuer {
    Issuer {
        nif: ISSUER_NIF.to_owned(),
        name: "Example SL".to_owned(),
        address: "Calle Real 5, 15001 A Coruña".to_owned(),
    }
}

/// Id of the customer that `seed` creates.
pub const CUSTOMER_ID: i64 = 1;

/// Company, two series and one customer, as most tests need.
pub async fn seed(pool: &PgPool) {
    sqlx::query(
        "INSERT INTO company (nif, name, address, postal_code, city)
         VALUES ($1, 'Example SL', 'Calle Real 5', '15001', 'A Coruña')",
    )
    .bind(ISSUER_NIF)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO series (code) VALUES ('A2026'), ('R2026')")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO contacts (name, nif, address, postal_code, city)
         VALUES ('Customer & Sons SA', 'A58818501', 'Calle Mayor 1', '28013', 'Madrid')",
    )
    .execute(pool)
    .await
    .unwrap();
}

pub const EMAIL: &str = "tester@example.com";

/// Where the test application is served; passkeys are bound to it.
pub const PUBLIC_URL: &str = "https://books.example.com";

/// Token of the session that `app` opens and `request` sends.
const TEST_TOKEN: &str = "test-session-token";

/// The application state, and the emails it sends.
pub fn state_with_outbox(pool: PgPool) -> (AppState, Arc<Mutex<Vec<Mail>>>) {
    let public_url = Url::parse(PUBLIC_URL).unwrap();
    let (mailer, outbox) = Mailer::memory();
    let state = AppState {
        pool,
        webauthn: Arc::new(config::webauthn(&public_url).unwrap()),
        config: Arc::new(Config {
            aeat_environment: AeatEnvironment::Test,
            public_url,
        }),
        mailer: Arc::new(mailer),
        // Nothing is sent unless a test sets a submitter.
        aeat: Arc::new(AeatService::new(AeatSettings::test(), None)),
    };
    (state, outbox)
}

pub fn state(pool: PgPool) -> AppState {
    state_with_outbox(pool).0
}

/// The application without any user or session.
pub fn app_signed_out(pool: PgPool) -> Router {
    vericount_server::app(state(pool), web_dir())
}

/// Creates a user with an open session; `request` sends its cookie.
pub async fn sign_in(pool: &PgPool) {
    let user_id: i64 = sqlx::query_scalar(
        "INSERT INTO users (email, activated_at) VALUES ($1, now()) RETURNING id",
    )
    .bind(EMAIL)
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO sessions (user_id, token_hash, expires_at)
         VALUES ($1, $2, now() + interval '1 hour')",
    )
    .bind(user_id)
    .bind(vericount_server::auth::token_hash(TEST_TOKEN))
    .execute(pool)
    .await
    .unwrap();
}

/// The application with a signed-in test user and the seed data.
pub async fn app(pool: PgPool) -> Router {
    sign_in(&pool).await;
    seed(&pool).await;
    app_signed_out(pool)
}

pub struct TestResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl TestResponse {
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&self.body)))
    }
}

pub async fn request(
    app: &Router,
    method: Method,
    path: &str,
    json: Option<Value>,
) -> TestResponse {
    let builder = Request::builder()
        .method(method)
        .uri(path)
        .header(COOKIE, format!("__Host-session={TEST_TOKEN}"))
        .header("x-csrf-protection", "1");
    let request = match json {
        Some(json) => builder
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(json.to_string())),
        None => builder.body(Body::empty()),
    }
    .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    TestResponse {
        status: response.status(),
        headers: response.headers().clone(),
        body: response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    }
}

pub async fn get(app: &Router, path: &str) -> TestResponse {
    request(app, Method::GET, path, None).await
}

pub async fn post(app: &Router, path: &str, json: Value) -> TestResponse {
    request(app, Method::POST, path, Some(json)).await
}
