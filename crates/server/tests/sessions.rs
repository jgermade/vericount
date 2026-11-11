//! Tests for sign-in, sessions and the protection of the API.

mod common;

use axum::{
    body::Body,
    http::{
        header::{CONTENT_TYPE, COOKIE, ORIGIN, SET_COOKIE},
        Method, Request, StatusCode,
    },
    Router,
};
use common::{PASSWORD, USERNAME};
use serde_json::json;
use sqlx::PgPool;
use tower::ServiceExt;
use vericounts_server::auth;

/// Sends a request with full control over its headers.
async fn send(
    app: &Router,
    method: Method,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<serde_json::Value>,
) -> axum::response::Response {
    let mut builder = Request::builder().method(method).uri(path);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = match body {
        Some(body) => builder
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string())),
        None => builder.body(Body::empty()),
    };
    app.clone().oneshot(request.unwrap()).await.unwrap()
}

fn credentials(password: &str) -> Option<serde_json::Value> {
    Some(json!({ "username": USERNAME, "password": password }))
}

const CSRF: (&str, &str) = ("x-csrf-protection", "1");

#[sqlx::test]
async fn sign_in_use_and_sign_out(pool: PgPool) {
    auth::create_user(&pool, USERNAME, PASSWORD).await.unwrap();
    let app = common::app_signed_out(pool.clone());

    // Without a session the API is closed, but the web client is served.
    for path in [
        "/api/invoices",
        "/api/chain/verification",
        "/api/aeat",
        "/api/session",
    ] {
        let response = send(&app, Method::GET, path, &[], None).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
    }
    assert_eq!(
        send(&app, Method::GET, "/", &[], None).await.status(),
        StatusCode::OK
    );

    let wrong = send(
        &app,
        Method::POST,
        "/api/session",
        &[CSRF],
        credentials("wrong password"),
    )
    .await;
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
    assert!(wrong.headers().get(SET_COOKIE).is_none());

    let response = send(
        &app,
        Method::POST,
        "/api/session",
        &[CSRF],
        credentials(PASSWORD),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let set_cookie = response.headers()[SET_COOKIE].to_str().unwrap().to_owned();
    for attribute in ["HttpOnly", "Secure", "SameSite=Strict", "Path=/"] {
        assert!(set_cookie.contains(attribute), "{attribute}");
    }
    let cookie = set_cookie.split(';').next().unwrap().to_owned();
    assert!(cookie.starts_with("__Host-session="));
    let session = [(COOKIE.as_str(), cookie.as_str())];

    // The token itself is not in the database.
    let token = cookie.split_once('=').unwrap().1;
    let stored: String = sqlx::query_scalar("SELECT token_hash FROM sessions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_ne!(stored, token);
    assert_eq!(stored, auth::token_hash(token));

    let response = send(&app, Method::GET, "/api/invoices", &session, None).await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = send(&app, Method::GET, "/api/session", &session, None).await;
    assert_eq!(response.status(), StatusCode::OK);

    let with_csrf = [session[0], CSRF];
    let response = send(&app, Method::DELETE, "/api/session", &with_csrf, None).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(response.headers()[SET_COOKIE]
        .to_str()
        .unwrap()
        .contains("Max-Age=0"));
    let response = send(&app, Method::GET, "/api/invoices", &session, None).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let kinds: Vec<String> = sqlx::query_scalar("SELECT kind FROM events ORDER BY id")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(
        kinds,
        ["session.login_failed", "session.opened", "session.closed"]
    );
}

#[sqlx::test]
async fn state_changing_requests_need_the_csrf_header_and_origin(pool: PgPool) {
    auth::create_user(&pool, USERNAME, PASSWORD).await.unwrap();
    let app = common::app_signed_out(pool);

    let response = send(
        &app,
        Method::POST,
        "/api/session",
        &[],
        credentials(PASSWORD),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let cross_site = [
        CSRF,
        (ORIGIN.as_str(), "https://evil.example"),
        ("host", "app.example"),
    ];
    let response = send(
        &app,
        Method::POST,
        "/api/session",
        &cross_site,
        credentials(PASSWORD),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let same_site = [
        CSRF,
        (ORIGIN.as_str(), "https://app.example"),
        ("host", "app.example"),
    ];
    let response = send(
        &app,
        Method::POST,
        "/api/session",
        &same_site,
        credentials(PASSWORD),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
}

#[sqlx::test]
async fn expired_revoked_and_disabled_sessions_are_refused(pool: PgPool) {
    let user_id = auth::create_user(&pool, USERNAME, PASSWORD).await.unwrap();
    let app = common::app_signed_out(pool.clone());
    let token = auth::create_session(&pool, user_id, "test").await.unwrap();
    let cookie = format!("__Host-session={token}");
    let session = [(COOKIE.as_str(), cookie.as_str())];
    let status = || async {
        send(&app, Method::GET, "/api/invoices", &session, None)
            .await
            .status()
    };
    assert_eq!(status().await, StatusCode::OK);

    sqlx::query("UPDATE sessions SET expires_at = now() - interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(status().await, StatusCode::UNAUTHORIZED);

    sqlx::query("UPDATE sessions SET expires_at = now() + interval '1 hour'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(status().await, StatusCode::OK);
    sqlx::query("UPDATE users SET disabled_at = now()")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(status().await, StatusCode::UNAUTHORIZED);

    // A disabled user cannot sign in either.
    let response = send(
        &app,
        Method::POST,
        "/api/session",
        &[CSRF],
        credentials(PASSWORD),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[sqlx::test]
async fn repeated_failures_block_sign_in_for_a_while(pool: PgPool) {
    auth::create_user(&pool, USERNAME, PASSWORD).await.unwrap();
    let app = common::app_signed_out(pool);
    for _ in 0..5 {
        let response = send(
            &app,
            Method::POST,
            "/api/session",
            &[CSRF],
            credentials("wrong password"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    // Even the right password is refused while the user is blocked.
    let response = send(
        &app,
        Method::POST,
        "/api/session",
        &[CSRF],
        credentials(PASSWORD),
    )
    .await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[sqlx::test]
async fn weak_passwords_and_bad_usernames_are_refused(pool: PgPool) {
    assert!(auth::create_user(&pool, USERNAME, "short").await.is_err());
    assert!(auth::create_user(&pool, "Not Valid", PASSWORD)
        .await
        .is_err());
    auth::create_user(&pool, USERNAME, PASSWORD).await.unwrap();
    assert!(auth::create_user(&pool, USERNAME, PASSWORD).await.is_err());
}
