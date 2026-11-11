#![allow(dead_code)]

use std::{path::PathBuf, sync::Arc};

use axum::{
    body::Body,
    http::{header::CONTENT_TYPE, HeaderMap, Method, Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use serde_json::Value;
use sqlx::PgPool;
use tower::ServiceExt;
use vericounts_core::qr::AeatEnvironment;
use vericounts_server::{
    config::{Config, Issuer},
    AppState,
};

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
    }
}

pub fn app(pool: PgPool) -> Router {
    let state = AppState {
        pool,
        config: Arc::new(Config {
            issuer: issuer(),
            aeat_environment: AeatEnvironment::Test,
        }),
    };
    vericounts_server::app(state, web_dir())
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
    let builder = Request::builder().method(method).uri(path);
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
