//! Users, sign-in codes, sessions and request authentication.
//!
//! There are no passwords. Users are invited by email address; they sign
//! up and sign in with a one-time code sent to that address, or with a
//! passkey (see [`crate::passkeys`]). Signing up is the first sign-in of
//! an invited user.
//!
//! A session is a random token kept in an `HttpOnly`, `Secure`,
//! `SameSite=Strict` cookie; only its SHA-256 is stored. Requests that
//! change state must also carry a custom header, which browsers do not
//! let other sites send, and an `Origin` that matches the host.

use axum::{
    extract::{Request, State},
    http::{
        header::{COOKIE, HOST, ORIGIN},
        HeaderMap, Method, StatusCode,
    },
    middleware::Next,
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::PgPool;

use crate::{api::ApiError, mail, AppState};

pub const SESSION_COOKIE: &str = "__Host-session";
pub const CSRF_HEADER: &str = "x-csrf-protection";
pub const SESSION_DAYS: i32 = 7;

/// How long a sign-in code is valid.
pub const CODE_MINUTES: i64 = 10;
/// Wrong guesses allowed per code; after that the code is useless.
pub const CODE_ATTEMPTS: i32 = 5;
/// Codes sent to one user per hour, and seconds between two of them.
pub const CODES_PER_HOUR: i64 = 5;
pub const CODE_INTERVAL_SECONDS: i64 = 60;

/// The authenticated user of a request.
#[derive(Debug, Clone)]
pub struct CurrentUser {
    pub id: i64,
    pub email: String,
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

/// What is stored for a sign-in code. The user id is mixed in so equal
/// codes of different users do not look alike.
pub fn code_hash(user_id: i64, code: &str) -> String {
    token_hash(&format!("{user_id}:{code}"))
}

/// A random six-digit code.
fn new_code() -> String {
    format!("{:06}", rand::random_range(0..1_000_000u32))
}

/// Trims and lowercases an email address and checks its shape. Whether it
/// exists is only known when the code arrives.
pub fn normalize_email(email: &str) -> Option<String> {
    let email = email.trim().to_lowercase();
    let (local, domain) = email.split_once('@')?;
    let valid = !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !domain.contains('@')
        && email.len() <= 254
        && !email.chars().any(|c| c.is_whitespace() || c.is_control());
    valid.then_some(email)
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

#[derive(Debug)]
pub enum InviteError {
    InvalidEmail,
    AlreadyInvited,
    Database(sqlx::Error),
}

impl From<sqlx::Error> for InviteError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error)
    }
}

/// Lets an email address sign up. A disabled user with that address is
/// enabled again. Returns the user id and the normalised address.
pub async fn invite(
    pool: &PgPool,
    email: &str,
    invited_by: Option<i64>,
) -> Result<(i64, String), InviteError> {
    let email = normalize_email(email).ok_or(InviteError::InvalidEmail)?;
    let id: Option<i64> = sqlx::query_scalar(
        "INSERT INTO users (email, invited_by) VALUES ($1, $2)
         ON CONFLICT (email) DO UPDATE SET disabled_at = NULL, invited_by = $2
             WHERE users.disabled_at IS NOT NULL
         RETURNING id",
    )
    .bind(&email)
    .bind(invited_by)
    .fetch_optional(pool)
    .await?;
    let id = id.ok_or(InviteError::AlreadyInvited)?;
    log_event(pool, invited_by.or(Some(id)), "user.invited", &email).await;
    Ok((id, email))
}

/// A user as listed in Settings.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct UserInfo {
    pub id: i64,
    pub email: String,
    pub invited_by: Option<String>,
    /// When the user signed up; empty while the invitation is pending.
    pub activated_at: Option<DateTime<Utc>>,
    pub passkeys: i64,
}

pub async fn list_users(pool: &PgPool) -> Result<Vec<UserInfo>, sqlx::Error> {
    sqlx::query_as(
        "SELECT u.id, u.email, i.email AS invited_by, u.activated_at,
                (SELECT count(*) FROM passkeys p WHERE p.user_id = u.id) AS passkeys
         FROM users u LEFT JOIN users i ON i.id = u.invited_by
         WHERE u.disabled_at IS NULL
         ORDER BY u.email",
    )
    .fetch_all(pool)
    .await
}

/// Disables a user and closes their sessions. Returns false if there is
/// no such enabled user.
pub async fn disable_user(pool: &PgPool, user_id: i64) -> Result<bool, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let disabled =
        sqlx::query("UPDATE users SET disabled_at = now() WHERE id = $1 AND disabled_at IS NULL")
            .bind(user_id)
            .execute(&mut *tx)
            .await?
            .rows_affected()
            == 1;
    sqlx::query("UPDATE sessions SET revoked_at = now() WHERE user_id = $1 AND revoked_at IS NULL")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(disabled)
}

/// Sends a sign-in code to `email` if it belongs to an enabled user.
///
/// The caller learns nothing either way: unknown addresses and requests
/// over the limits are silently ignored, so the answer does not reveal
/// who has an account.
pub async fn request_code(state: &AppState, email: &str) -> Result<(), sqlx::Error> {
    let Some(email) = normalize_email(email) else {
        return Ok(());
    };
    let user: Option<(i64, i64, bool)> = sqlx::query_as(
        "SELECT u.id,
                count(c.id) FILTER (WHERE c.created_at > now() - interval '1 hour'),
                coalesce(bool_or(c.created_at > now() - make_interval(secs => $2)), false)
         FROM users u LEFT JOIN login_codes c ON c.user_id = u.id
         WHERE u.email = $1 AND u.disabled_at IS NULL
         GROUP BY u.id",
    )
    .bind(&email)
    .bind(CODE_INTERVAL_SECONDS as f64)
    .fetch_optional(&state.pool)
    .await?;
    let Some((user_id, sent_last_hour, sent_just_now)) = user else {
        return Ok(());
    };
    if sent_last_hour >= CODES_PER_HOUR || sent_just_now {
        log_event(&state.pool, Some(user_id), "session.code_limited", "").await;
        return Ok(());
    }
    let code = new_code();
    sqlx::query(
        "INSERT INTO login_codes (user_id, code_hash, expires_at)
         VALUES ($1, $2, now() + make_interval(mins => $3))",
    )
    .bind(user_id)
    .bind(code_hash(user_id, &code))
    .bind(CODE_MINUTES as i32)
    .execute(&state.pool)
    .await?;
    state
        .mailer
        .dispatch(mail::sign_in_code(&email, &code, CODE_MINUTES));
    log_event(&state.pool, Some(user_id), "session.code_sent", "").await;
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
pub enum LoginError {
    InvalidCode,
    Internal,
}

impl From<sqlx::Error> for LoginError {
    fn from(error: sqlx::Error) -> Self {
        eprintln!("database error: {error}");
        Self::Internal
    }
}

/// A session just opened.
#[derive(Debug)]
pub struct SignedIn {
    pub token: String,
    pub user_id: i64,
    pub email: String,
    /// True when this sign-in is the user's sign-up.
    pub first_sign_in: bool,
}

/// Checks a sign-in code and opens a session.
///
/// Only the latest code of the user counts, and only while it is unused,
/// unexpired and has guesses left. Every guess is counted before the code
/// is compared, so parallel requests cannot try more than
/// [`CODE_ATTEMPTS`] codes.
pub async fn sign_in_with_code(
    state: &AppState,
    email: &str,
    code: &str,
    user_agent: &str,
) -> Result<SignedIn, LoginError> {
    let email = normalize_email(email).ok_or(LoginError::InvalidCode)?;
    let latest: Option<(i64, i64, String)> = sqlx::query_as(
        "SELECT c.id, c.user_id, c.code_hash
         FROM login_codes c JOIN users u ON u.id = c.user_id
         WHERE u.email = $1 AND u.disabled_at IS NULL
         ORDER BY c.created_at DESC, c.id DESC
         LIMIT 1",
    )
    .bind(&email)
    .fetch_optional(&state.pool)
    .await?;
    let Some((code_id, user_id, stored_hash)) = latest else {
        return Err(LoginError::InvalidCode);
    };
    let counted = sqlx::query(
        "UPDATE login_codes SET attempts = attempts + 1
         WHERE id = $1 AND attempts < $2 AND used_at IS NULL AND expires_at > now()",
    )
    .bind(code_id)
    .bind(CODE_ATTEMPTS)
    .execute(&state.pool)
    .await?
    .rows_affected()
        == 1;
    let matches = code_hash(user_id, code.trim()) == stored_hash;
    if !counted || !matches {
        log_event(&state.pool, Some(user_id), "session.code_failed", "").await;
        return Err(LoginError::InvalidCode);
    }
    let used =
        sqlx::query("UPDATE login_codes SET used_at = now() WHERE id = $1 AND used_at IS NULL")
            .bind(code_id)
            .execute(&state.pool)
            .await?
            .rows_affected()
            == 1;
    if !used {
        return Err(LoginError::InvalidCode);
    }
    open_session(&state.pool, user_id, email, user_agent, "code").await
}

/// Opens a session for a user that has just proved who they are, and
/// marks the user as signed up if this is their first time.
pub async fn open_session(
    pool: &PgPool,
    user_id: i64,
    email: String,
    user_agent: &str,
    method: &str,
) -> Result<SignedIn, LoginError> {
    let first_sign_in =
        sqlx::query("UPDATE users SET activated_at = now() WHERE id = $1 AND activated_at IS NULL")
            .bind(user_id)
            .execute(pool)
            .await?
            .rows_affected()
            == 1;
    if first_sign_in {
        log_event(pool, Some(user_id), "user.signed_up", &email).await;
    }
    let token = create_session(pool, user_id, user_agent).await?;
    log_event(pool, Some(user_id), "session.opened", method).await;
    Ok(SignedIn {
        token,
        user_id,
        email,
        first_sign_in,
    })
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
        "SELECT u.id, u.email, s.id
         FROM sessions s JOIN users u ON u.id = s.user_id
         WHERE s.token_hash = $1 AND s.revoked_at IS NULL AND s.expires_at > now()
           AND u.disabled_at IS NULL",
    )
    .bind(token_hash(token))
    .fetch_optional(pool)
    .await
    .ok()?;
    row.map(|(id, email, session_id)| CurrentUser {
        id,
        email,
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
    fn codes_have_six_digits() {
        for _ in 0..100 {
            let code = new_code();
            assert_eq!(code.len(), 6);
            assert!(code.chars().all(|c| c.is_ascii_digit()));
        }
        assert_ne!(code_hash(1, "123456"), code_hash(2, "123456"));
    }

    #[test]
    fn emails_are_normalised() {
        assert_eq!(
            normalize_email("  Ana.Garcia@Example.ES ").as_deref(),
            Some("ana.garcia@example.es")
        );
        for invalid in [
            "",
            "ana",
            "@example.es",
            "ana@",
            "ana@example",
            "a b@example.es",
        ] {
            assert_eq!(normalize_email(invalid), None, "{invalid}");
        }
        assert_eq!(normalize_email("ana@example.es@x.es"), None);
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
