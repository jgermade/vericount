//! Users, sessions and request authentication.
//!
//! A session is a random token kept in an `HttpOnly`, `Secure`,
//! `SameSite=Strict` cookie; only its SHA-256 is stored. Requests that
//! change state must also carry a custom header, which browsers do not
//! let other sites send, and an `Origin` that matches the host.

use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use axum::{
    extract::{Request, State},
    http::{
        header::{COOKIE, HOST, ORIGIN},
        HeaderMap, Method, StatusCode,
    },
    middleware::Next,
    response::{IntoResponse, Response},
};
use sha2::{Digest, Sha256};
use sqlx::PgPool;

use crate::{api::ApiError, AppState};

pub const SESSION_COOKIE: &str = "__Host-session";
pub const CSRF_HEADER: &str = "x-csrf-protection";
pub const SESSION_DAYS: i32 = 7;
pub const MIN_PASSWORD_LENGTH: usize = 12;

const MAX_FAILED_LOGINS: u32 = 5;
const FAILED_LOGIN_WINDOW: Duration = Duration::from_secs(300);

/// The authenticated user of a request.
#[derive(Debug, Clone)]
pub struct CurrentUser {
    pub id: i64,
    pub username: String,
    pub session_id: i64,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn new_token() -> String {
    hex(&rand::random::<[u8; 32]>())
}

pub fn token_hash(token: &str) -> String {
    hex(&Sha256::digest(token.as_bytes()))
}

pub fn hash_password(password: &str) -> Result<String, String> {
    if password.chars().count() < MIN_PASSWORD_LENGTH {
        return Err(format!(
            "the password must have at least {MIN_PASSWORD_LENGTH} characters"
        ));
    }
    Argon2::default()
        .hash_password(password.as_bytes(), &SaltString::generate(&mut OsRng))
        .map(|hash| hash.to_string())
        .map_err(|e| e.to_string())
}

fn verify_password(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash)
        .map(|parsed| {
            Argon2::default()
                .verify_password(password.as_bytes(), &parsed)
                .is_ok()
        })
        .unwrap_or(false)
}

/// Limits failed logins per username, in memory.
#[derive(Debug, Default)]
pub struct LoginThrottle {
    failures: Mutex<HashMap<String, (u32, Instant)>>,
}

impl LoginThrottle {
    fn blocked(&self, username: &str) -> bool {
        let mut failures = self.failures.lock().unwrap();
        failures.retain(|_, (_, since)| since.elapsed() < FAILED_LOGIN_WINDOW);
        failures
            .get(username)
            .is_some_and(|(count, _)| *count >= MAX_FAILED_LOGINS)
    }

    fn record_failure(&self, username: &str) {
        let mut failures = self.failures.lock().unwrap();
        // Bound the memory an attacker can use with made-up usernames.
        if failures.len() >= 10_000 {
            failures.clear();
        }
        failures
            .entry(username.to_owned())
            .or_insert((0, Instant::now()))
            .0 += 1;
    }

    fn clear(&self, username: &str) {
        self.failures.lock().unwrap().remove(username);
    }
}

/// Appends a row to the event log. Failures are logged, not returned: an
/// event must never make the action it describes fail halfway.
pub async fn log_event(pool: &PgPool, user_id: Option<i64>, kind: &str, detail: &str) {
    let result = sqlx::query("INSERT INTO events (user_id, kind, detail) VALUES ($1, $2, $3)")
        .bind(user_id)
        .bind(kind)
        .bind(detail)
        .execute(pool)
        .await;
    if let Err(error) = result {
        eprintln!("cannot log event {kind}: {error}");
    }
}

pub async fn create_user(pool: &PgPool, username: &str, password: &str) -> Result<i64, String> {
    let hash = hash_password(password)?;
    sqlx::query_scalar("INSERT INTO users (username, password_hash) VALUES ($1, $2) RETURNING id")
        .bind(username)
        .bind(hash)
        .fetch_one(pool)
        .await
        .map_err(|e| e.to_string())
}

/// Inserts a session for a user and returns its token.
pub async fn create_session(
    pool: &PgPool,
    user_id: i64,
    user_agent: &str,
) -> Result<String, sqlx::Error> {
    let token = new_token();
    sqlx::query(
        "INSERT INTO sessions (user_id, token_hash, user_agent, expires_at)
         VALUES ($1, $2, $3, now() + make_interval(days => $4))",
    )
    .bind(user_id)
    .bind(token_hash(&token))
    .bind(user_agent.chars().take(200).collect::<String>())
    .bind(SESSION_DAYS)
    .execute(pool)
    .await?;
    Ok(token)
}

#[derive(Debug, PartialEq, Eq)]
pub enum LoginError {
    InvalidCredentials,
    TooManyAttempts,
    Internal,
}

/// Checks the credentials and opens a session. Returns its token.
pub async fn login(
    state: &AppState,
    username: &str,
    password: &str,
    user_agent: &str,
) -> Result<String, LoginError> {
    let username = username.trim().to_lowercase();
    if state.throttle.blocked(&username) {
        return Err(LoginError::TooManyAttempts);
    }
    let user: Option<(i64, String)> = sqlx::query_as(
        "SELECT id, password_hash FROM users WHERE username = $1 AND disabled_at IS NULL",
    )
    .bind(&username)
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| LoginError::Internal)?;

    // An unknown user costs the same time as a wrong password.
    static DUMMY_HASH: OnceLock<String> = OnceLock::new();
    let dummy = || {
        DUMMY_HASH
            .get_or_init(|| hash_password("dummy password for timing").unwrap_or_default())
            .clone()
    };
    let (user_id, hash) = match &user {
        Some((id, hash)) => (Some(*id), hash.clone()),
        None => (None, dummy()),
    };
    let password = password.to_owned();
    let valid = tokio::task::spawn_blocking(move || verify_password(&password, &hash))
        .await
        .unwrap_or(false);

    match user_id.filter(|_| valid) {
        Some(user_id) => {
            state.throttle.clear(&username);
            let token = create_session(&state.pool, user_id, user_agent)
                .await
                .map_err(|_| LoginError::Internal)?;
            log_event(&state.pool, Some(user_id), "session.opened", "").await;
            Ok(token)
        }
        None => {
            state.throttle.record_failure(&username);
            log_event(&state.pool, user_id, "session.login_failed", &username).await;
            Err(LoginError::InvalidCredentials)
        }
    }
}

pub async fn revoke_session(pool: &PgPool, session_id: i64) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE sessions SET revoked_at = now() WHERE id = $1 AND revoked_at IS NULL")
        .bind(session_id)
        .execute(pool)
        .await?;
    Ok(())
}

fn session_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == SESSION_COOKIE)
        .map(|(_, value)| value)
}

pub async fn authenticate(pool: &PgPool, headers: &HeaderMap) -> Option<CurrentUser> {
    let token = session_token(headers)?;
    let row: Option<(i64, String, i64)> = sqlx::query_as(
        "SELECT u.id, u.username, s.id
         FROM sessions s JOIN users u ON u.id = s.user_id
         WHERE s.token_hash = $1 AND s.revoked_at IS NULL AND s.expires_at > now()
           AND u.disabled_at IS NULL",
    )
    .bind(token_hash(token))
    .fetch_optional(pool)
    .await
    .ok()?;
    row.map(|(id, username, session_id)| CurrentUser {
        id,
        username,
        session_id,
    })
}

/// `Set-Cookie` value that stores (or, with `None`, removes) the session.
pub fn session_cookie(token: Option<&str>) -> String {
    let (value, max_age) = match token {
        Some(token) => (token, i64::from(SESSION_DAYS) * 24 * 60 * 60),
        None => ("", 0),
    };
    format!(
        "{SESSION_COOKIE}={value}; Path=/; Max-Age={max_age}; HttpOnly; Secure; SameSite=Strict"
    )
}

/// Refuses requests that change state unless they come from our own
/// pages: they need the custom header and, when the browser sends an
/// `Origin`, it must match the host.
fn csrf_check(method: &Method, headers: &HeaderMap) -> bool {
    if matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS) {
        return true;
    }
    if !headers.contains_key(CSRF_HEADER) {
        return false;
    }
    match (headers.get(ORIGIN), headers.get(HOST)) {
        (None, _) => true,
        (Some(origin), Some(host)) => origin
            .to_str()
            .ok()
            .and_then(|origin| origin.split_once("://"))
            .is_some_and(|(_, origin_host)| origin_host.as_bytes() == host.as_bytes()),
        (Some(_), None) => false,
    }
}

/// Middleware for every API route: CSRF check.
pub async fn csrf(request: Request, next: Next) -> Response {
    if !csrf_check(request.method(), request.headers()) {
        return ApiError::new(StatusCode::FORBIDDEN, "csrf", "request refused").into_response();
    }
    next.run(request).await
}

/// Middleware for protected API routes: requires a valid session and
/// makes the [`CurrentUser`] available to the handlers.
pub async fn require_session(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    match authenticate(&state.pool, request.headers()).await {
        Some(user) => {
            request.extensions_mut().insert(user);
            next.run(request).await
        }
        None => ApiError::new(
            StatusCode::UNAUTHORIZED,
            "unauthenticated",
            "sign in to continue",
        )
        .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn passwords_hash_and_verify() {
        assert!(hash_password("short").is_err());
        let hash = hash_password("correct horse battery").unwrap();
        assert!(hash.starts_with("$argon2id$"));
        assert!(verify_password("correct horse battery", &hash));
        assert!(!verify_password("wrong horse battery", &hash));
        assert!(!verify_password("anything", "not a hash"));
    }

    #[test]
    fn tokens_are_random_and_hashed() {
        let token = new_token();
        assert_eq!(token.len(), 64);
        assert_ne!(token, new_token());
        assert_eq!(token_hash(&token).len(), 64);
        assert_ne!(token_hash(&token), token);
    }

    #[test]
    fn csrf_rules() {
        let headers = |pairs: &[(&'static str, &'static str)]| {
            let mut map = HeaderMap::new();
            for (name, value) in pairs {
                map.insert(*name, HeaderValue::from_static(value));
            }
            map
        };
        assert!(csrf_check(&Method::GET, &headers(&[])));
        assert!(!csrf_check(&Method::POST, &headers(&[])));
        assert!(csrf_check(&Method::POST, &headers(&[(CSRF_HEADER, "1")])));
        let same = [
            (CSRF_HEADER, "1"),
            ("origin", "https://app.example"),
            ("host", "app.example"),
        ];
        assert!(csrf_check(&Method::POST, &headers(&same)));
        let cross = [
            (CSRF_HEADER, "1"),
            ("origin", "https://evil.example"),
            ("host", "app.example"),
        ];
        assert!(!csrf_check(&Method::DELETE, &headers(&cross)));
    }

    #[test]
    fn throttle_blocks_after_repeated_failures() {
        let throttle = LoginThrottle::default();
        for _ in 0..MAX_FAILED_LOGINS {
            assert!(!throttle.blocked("ana"));
            throttle.record_failure("ana");
        }
        assert!(throttle.blocked("ana"));
        assert!(!throttle.blocked("luis"));
        throttle.clear("ana");
        assert!(!throttle.blocked("ana"));
    }

    #[test]
    fn cookie_is_locked_down() {
        let cookie = session_cookie(Some("abc"));
        for part in [
            "__Host-session=abc",
            "HttpOnly",
            "Secure",
            "SameSite=Strict",
            "Path=/",
        ] {
            assert!(cookie.contains(part), "{part}");
        }
        assert!(session_cookie(None).contains("Max-Age=0"));
    }

    #[test]
    fn session_token_is_read_from_cookies() {
        let mut headers = HeaderMap::new();
        headers.insert(
            COOKIE,
            HeaderValue::from_static("a=1; __Host-session=tok; b=2"),
        );
        assert_eq!(session_token(&headers), Some("tok"));
        assert_eq!(session_token(&HeaderMap::new()), None);
    }
}
