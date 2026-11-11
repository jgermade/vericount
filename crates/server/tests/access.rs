//! Tests for sign-up and sign-in (email codes and passkeys), sessions,
//! invitations and the protection of the API.

mod common;

use std::{
    env,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    body::Body,
    http::{
        header::{CONTENT_TYPE, COOKIE, ORIGIN, SET_COOKIE},
        Method, Request, StatusCode,
    },
    Router,
};
use common::{authenticator::SoftPasskey, EMAIL, PUBLIC_URL};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;
use vericount_server::{
    access::{sessions, users},
    mail::{Mail, Mailer},
    AppState,
};

const CSRF: (&str, &str) = ("x-csrf-protection", "1");

struct Reply {
    status: StatusCode,
    set_cookie: Option<String>,
    json: Value,
}

impl Reply {
    /// The `name=value` part of the session cookie that was set.
    fn cookie(&self) -> String {
        let set_cookie = self.set_cookie.as_deref().expect("no Set-Cookie");
        set_cookie.split(';').next().unwrap().to_owned()
    }
}

/// Sends a request with full control over its headers.
async fn send(
    app: &Router,
    method: Method,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<Value>,
) -> Reply {
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
    let response = app.clone().oneshot(request.unwrap()).await.unwrap();
    let status = response.status();
    let set_cookie = response
        .headers()
        .get(SET_COOKIE)
        .map(|value| value.to_str().unwrap().to_owned());
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Reply {
        status,
        set_cookie,
        json,
    }
}

/// Sends a JSON request with the CSRF header.
async fn post(app: &Router, path: &str, extra: &[(&str, &str)], body: Value) -> Reply {
    let mut headers = vec![CSRF];
    headers.extend_from_slice(extra);
    send(app, Method::POST, path, &headers, Some(body)).await
}

struct TestApp {
    app: Router,
    outbox: Arc<Mutex<Vec<Mail>>>,
}

impl TestApp {
    fn new(state: AppState, outbox: Arc<Mutex<Vec<Mail>>>) -> Self {
        Self {
            app: vericount_server::app(state, common::web_dir()),
            outbox,
        }
    }

    fn signed_out(pool: PgPool) -> Self {
        let (state, outbox) = common::state_with_outbox(pool);
        Self::new(state, outbox)
    }

    fn mails(&self) -> Vec<Mail> {
        self.outbox.lock().unwrap().clone()
    }

    async fn request_code(&self, email: &str) -> StatusCode {
        post(
            &self.app,
            "/api/session/code",
            &[],
            json!({ "email": email }),
        )
        .await
        .status
    }

    async fn sign_in(&self, email: &str, code: &str) -> Reply {
        post(
            &self.app,
            "/api/session",
            &[],
            json!({ "email": email, "code": code }),
        )
        .await
    }

    /// The code in the last email sent.
    fn last_code(&self) -> String {
        code_in(&self.mails().last().expect("no email sent").body)
    }

    /// Signs in with a code and returns the session cookie.
    async fn signed_in_cookie(&self, email: &str) -> String {
        assert_eq!(self.request_code(email).await, StatusCode::ACCEPTED);
        let reply = self.sign_in(email, &self.last_code()).await;
        assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.json);
        reply.cookie()
    }
}

/// The six-digit code in the body of an email.
fn code_in(body: &str) -> String {
    body.split_whitespace()
        .find(|word| word.len() == 6 && word.chars().all(|c| c.is_ascii_digit()))
        .expect("no code in the email")
        .to_owned()
}

/// Moves the user's latest code back in time, as if `seconds` had passed.
async fn age_codes(pool: &PgPool, seconds: i32) {
    sqlx::query(
        "UPDATE login_codes SET created_at = created_at - make_interval(secs => $1),
                                expires_at = expires_at - make_interval(secs => $1)",
    )
    .bind(f64::from(seconds))
    .execute(pool)
    .await
    .unwrap();
}

async fn events(pool: &PgPool) -> Vec<String> {
    sqlx::query_scalar("SELECT kind FROM events ORDER BY id")
        .fetch_all(pool)
        .await
        .unwrap()
}

#[sqlx::test]
async fn invited_users_sign_up_and_sign_in_with_a_code(pool: PgPool) {
    common::seed(&pool).await;
    users::invite(&pool, "  Tester@Example.com ", None)
        .await
        .unwrap();
    let test = TestApp::signed_out(pool.clone());

    // Without a session the API is closed, but the web client is served.
    for path in [
        "/api/invoices",
        "/api/chain/verification",
        "/api/aeat",
        "/api/session",
        "/api/passkeys",
        "/api/users",
    ] {
        let reply = send(&test.app, Method::GET, path, &[], None).await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{path}");
    }
    let reply = send(&test.app, Method::GET, "/", &[], None).await;
    assert_eq!(reply.status, StatusCode::OK);

    assert_eq!(
        test.request_code("TESTER@example.com").await,
        StatusCode::ACCEPTED
    );
    let mails = test.mails();
    assert_eq!(mails.len(), 1);
    assert_eq!(mails[0].to, EMAIL);
    let code = code_in(&mails[0].body);
    assert!(mails[0].subject.contains(&code));

    // The code itself is not in the database.
    let stored: String = sqlx::query_scalar("SELECT code_hash FROM login_codes")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(!stored.contains(&code));

    let wrong = if code == "000000" { "111111" } else { "000000" };
    let reply = test.sign_in(EMAIL, wrong).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    assert_eq!(reply.json["error"]["code"], "invalid_code");
    assert!(reply.set_cookie.is_none());

    // The first sign-in is the sign-up.
    let reply = test.sign_in(EMAIL, &code).await;
    assert_eq!(reply.status, StatusCode::CREATED);
    assert_eq!(reply.json["email"], EMAIL);
    assert_eq!(reply.json["passkeys"], 0);
    assert_eq!(reply.json["first_sign_in"], true);
    let set_cookie = reply.set_cookie.clone().unwrap();
    for attribute in ["HttpOnly", "Secure", "SameSite=Strict", "Path=/"] {
        assert!(set_cookie.contains(attribute), "{attribute}");
    }
    let cookie = reply.cookie();
    assert!(cookie.starts_with("__Host-session="));
    let session = [(COOKIE.as_str(), cookie.as_str())];

    // The session token is stored hashed.
    let token = cookie.split_once('=').unwrap().1;
    let stored: String = sqlx::query_scalar("SELECT token_hash FROM sessions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stored, sessions::token_hash(token));

    let reply = send(&test.app, Method::GET, "/api/invoices", &session, None).await;
    assert_eq!(reply.status, StatusCode::OK);
    let reply = send(&test.app, Method::GET, "/api/session", &session, None).await;
    assert_eq!(reply.json, json!({ "email": EMAIL, "passkeys": 0 }));

    // A code works once.
    let reply = test.sign_in(EMAIL, &code).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);

    let with_csrf = [session[0], CSRF];
    let reply = send(&test.app, Method::DELETE, "/api/session", &with_csrf, None).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    assert!(reply.set_cookie.unwrap().contains("Max-Age=0"));
    let reply = send(&test.app, Method::GET, "/api/invoices", &session, None).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);

    // Later sign-ins are not sign-ups.
    age_codes(&pool, 120).await;
    assert_eq!(test.request_code(EMAIL).await, StatusCode::ACCEPTED);
    let reply = test.sign_in(EMAIL, &test.last_code()).await;
    assert_eq!(reply.status, StatusCode::CREATED);
    assert_eq!(reply.json.get("first_sign_in"), None);

    assert_eq!(
        events(&pool).await,
        [
            "user.invited",
            "session.code_sent",
            "session.code_failed",
            "user.signed_up",
            "session.opened",
            "session.code_failed",
            "session.closed",
            "session.code_sent",
            "session.opened",
        ]
    );
}

#[sqlx::test]
async fn nobody_else_gets_a_code_and_the_answer_does_not_tell(pool: PgPool) {
    let (user_id, _) = users::invite(&pool, EMAIL, None).await.unwrap();
    users::invite(&pool, "gone@example.com", None)
        .await
        .unwrap();
    sqlx::query("UPDATE users SET disabled_at = now() WHERE email = 'gone@example.com'")
        .execute(&pool)
        .await
        .unwrap();
    let test = TestApp::signed_out(pool.clone());

    for email in ["stranger@example.com", "gone@example.com", "not an email"] {
        assert_eq!(
            test.request_code(email).await,
            StatusCode::ACCEPTED,
            "{email}"
        );
        let reply = test.sign_in(email, "123456").await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{email}");
    }
    assert!(test.mails().is_empty());

    // A disabled user cannot use a code sent before.
    assert_eq!(test.request_code(EMAIL).await, StatusCode::ACCEPTED);
    sqlx::query("UPDATE users SET disabled_at = now() WHERE id = $1")
        .bind(user_id)
        .execute(&pool)
        .await
        .unwrap();
    let reply = test.sign_in(EMAIL, &test.last_code()).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
}

#[sqlx::test]
async fn codes_expire_run_out_of_guesses_and_are_replaced(pool: PgPool) {
    users::invite(&pool, EMAIL, None).await.unwrap();
    let test = TestApp::signed_out(pool.clone());

    // Expired.
    test.request_code(EMAIL).await;
    let code = test.last_code();
    age_codes(&pool, 11 * 60).await;
    assert_eq!(
        test.sign_in(EMAIL, &code).await.status,
        StatusCode::UNAUTHORIZED
    );

    // Five wrong guesses burn the code, even for the right one after them.
    test.request_code(EMAIL).await;
    let code = test.last_code();
    let wrong = if code == "000000" { "111111" } else { "000000" };
    for _ in 0..sessions::CODE_ATTEMPTS {
        assert_eq!(
            test.sign_in(EMAIL, wrong).await.status,
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        test.sign_in(EMAIL, &code).await.status,
        StatusCode::UNAUTHORIZED
    );

    // A new code replaces the previous one.
    age_codes(&pool, 120).await;
    test.request_code(EMAIL).await;
    let first = test.last_code();
    age_codes(&pool, 120).await;
    test.request_code(EMAIL).await;
    let second = test.last_code();
    if first != second {
        assert_eq!(
            test.sign_in(EMAIL, &first).await.status,
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        test.sign_in(EMAIL, &second).await.status,
        StatusCode::CREATED
    );
}

#[sqlx::test]
async fn codes_are_sent_at_most_once_a_minute_and_five_an_hour(pool: PgPool) {
    users::invite(&pool, EMAIL, None).await.unwrap();
    let test = TestApp::signed_out(pool.clone());

    test.request_code(EMAIL).await;
    assert_eq!(test.request_code(EMAIL).await, StatusCode::ACCEPTED);
    assert_eq!(test.mails().len(), 1, "a second code within a minute");

    for _ in 1..sessions::CODES_PER_HOUR {
        age_codes(&pool, 61).await;
        test.request_code(EMAIL).await;
    }
    assert_eq!(test.mails().len(), sessions::CODES_PER_HOUR as usize);
    age_codes(&pool, 61).await;
    test.request_code(EMAIL).await;
    assert_eq!(
        test.mails().len(),
        sessions::CODES_PER_HOUR as usize,
        "over the hourly limit"
    );
    assert!(events(&pool)
        .await
        .contains(&"session.code_limited".to_owned()));
}

#[sqlx::test]
async fn state_changing_requests_need_the_csrf_header_and_origin(pool: PgPool) {
    users::invite(&pool, EMAIL, None).await.unwrap();
    let test = TestApp::signed_out(pool);
    let body = Some(json!({ "email": EMAIL }));

    let reply = send(
        &test.app,
        Method::POST,
        "/api/session/code",
        &[],
        body.clone(),
    )
    .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);

    let cross_site = [
        CSRF,
        (ORIGIN.as_str(), "https://evil.example"),
        ("host", "app.example"),
    ];
    let reply = send(
        &test.app,
        Method::POST,
        "/api/session/code",
        &cross_site,
        body.clone(),
    )
    .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert!(test.mails().is_empty());

    let same_site = [
        CSRF,
        (ORIGIN.as_str(), "https://app.example"),
        ("host", "app.example"),
    ];
    let reply = send(
        &test.app,
        Method::POST,
        "/api/session/code",
        &same_site,
        body,
    )
    .await;
    assert_eq!(reply.status, StatusCode::ACCEPTED);
    assert_eq!(test.mails().len(), 1);
}

#[sqlx::test]
async fn expired_revoked_and_disabled_sessions_are_refused(pool: PgPool) {
    common::seed(&pool).await;
    let (user_id, _) = users::invite(&pool, EMAIL, None).await.unwrap();
    let app = common::app_signed_out(pool.clone());
    let token = sessions::create_session(&pool, user_id, "test")
        .await
        .unwrap();
    let cookie = format!("__Host-session={token}");
    let session = [(COOKIE.as_str(), cookie.as_str())];
    let status = || async {
        send(&app, Method::GET, "/api/invoices", &session, None)
            .await
            .status
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
    assert!(users::disable_user(&pool, user_id).await.unwrap());
    assert_eq!(status().await, StatusCode::UNAUTHORIZED);
}

#[sqlx::test]
async fn signed_in_users_invite_and_remove_others(pool: PgPool) {
    users::invite(&pool, EMAIL, None).await.unwrap();
    let test = TestApp::signed_out(pool.clone());
    let cookie = test.signed_in_cookie(EMAIL).await;
    let session = [(COOKIE.as_str(), cookie.as_str())];

    let reply = post(
        &test.app,
        "/api/users",
        &session,
        json!({ "email": "Ana@Example.com" }),
    )
    .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.json);
    let invitation = test.mails().last().unwrap().clone();
    assert_eq!(invitation.to, "ana@example.com");
    assert!(invitation.body.contains(EMAIL));
    assert!(invitation.body.contains(PUBLIC_URL));

    let reply = post(
        &test.app,
        "/api/users",
        &session,
        json!({ "email": "ana@example.com" }),
    )
    .await;
    assert_eq!(reply.status, StatusCode::CONFLICT);
    assert_eq!(reply.json["error"]["code"], "user_exists");
    let reply = post(&test.app, "/api/users", &session, json!({ "email": "ana" })).await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(reply.json["error"]["field"], "email");

    let reply = send(&test.app, Method::GET, "/api/users", &session, None).await;
    let users = reply.json.as_array().unwrap().clone();
    assert_eq!(users.len(), 2);
    let ana = users
        .iter()
        .find(|u| u["email"] == "ana@example.com")
        .unwrap();
    assert_eq!(ana["invited_by"], EMAIL);
    assert_eq!(ana["activated_at"], Value::Null);
    let me = users.iter().find(|u| u["email"] == EMAIL).unwrap();

    // Ana signs up, then loses access.
    let ana_cookie = test.signed_in_cookie("ana@example.com").await;
    let with_csrf = [session[0], CSRF];
    let me_path = format!("/api/users/{}", me["id"]);
    let reply = send(&test.app, Method::DELETE, &me_path, &with_csrf, None).await;
    assert_eq!(reply.status, StatusCode::CONFLICT);
    let ana_path = format!("/api/users/{}", ana["id"]);
    let reply = send(&test.app, Method::DELETE, &ana_path, &with_csrf, None).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    let ana_session = [(COOKIE.as_str(), ana_cookie.as_str())];
    let reply = send(&test.app, Method::GET, "/api/session", &ana_session, None).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);

    // Inviting her again gives access back.
    let reply = post(
        &test.app,
        "/api/users",
        &session,
        json!({ "email": "ana@example.com" }),
    )
    .await;
    assert_eq!(reply.status, StatusCode::CREATED);
}

/// Registers a passkey for the signed-in user and returns it.
async fn register_passkey(test: &TestApp, cookie: &str) -> SoftPasskey {
    let session = [(COOKIE.as_str(), cookie)];
    let challenge = post(&test.app, "/api/passkeys/options", &session, json!({})).await;
    assert_eq!(challenge.status, StatusCode::OK, "{}", challenge.json);
    let options = &challenge.json["options"];
    assert_eq!(options["publicKey"]["rp"]["id"], "books.example.com");
    assert_eq!(
        options["publicKey"]["authenticatorSelection"]["residentKey"],
        "required"
    );
    assert_eq!(
        options["publicKey"]["authenticatorSelection"]["userVerification"],
        "required"
    );
    let mut passkey = SoftPasskey::new(PUBLIC_URL);
    let body = json!({
        "ceremony": challenge.json["ceremony"],
        "name": "Phone",
        "credential": passkey.register(options),
    });
    let reply = post(&test.app, "/api/passkeys", &session, body).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.json);
    assert_eq!(reply.json["name"], "Phone");
    passkey
}

async fn passkey_sign_in(test: &TestApp, passkey: &mut SoftPasskey) -> Reply {
    let challenge = post(&test.app, "/api/session/passkey/options", &[], json!({})).await;
    assert_eq!(challenge.status, StatusCode::OK, "{}", challenge.json);
    assert_eq!(challenge.json["options"].get("mediation"), None);
    let body = json!({
        "ceremony": challenge.json["ceremony"],
        "credential": passkey.sign_in(&challenge.json["options"]),
    });
    post(&test.app, "/api/session/passkey", &[], body).await
}

#[sqlx::test]
async fn passkeys_are_registered_and_open_sessions(pool: PgPool) {
    users::invite(&pool, EMAIL, None).await.unwrap();
    let test = TestApp::signed_out(pool.clone());
    let cookie = test.signed_in_cookie(EMAIL).await;
    let mut passkey = register_passkey(&test, &cookie).await;

    let session = [(COOKIE.as_str(), cookie.as_str())];
    let reply = send(&test.app, Method::GET, "/api/session", &session, None).await;
    assert_eq!(reply.json["passkeys"], 1);

    // Signing in needs neither email nor code.
    let mails_before = test.mails().len();
    let reply = passkey_sign_in(&test, &mut passkey).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.json);
    assert_eq!(reply.json["email"], EMAIL);
    assert_eq!(reply.json["passkeys"], 1);
    assert_eq!(test.mails().len(), mails_before);
    let passkey_cookie = reply.cookie();
    let passkey_session = [(COOKIE.as_str(), passkey_cookie.as_str())];
    let reply = send(
        &test.app,
        Method::GET,
        "/api/invoices",
        &passkey_session,
        None,
    )
    .await;
    assert_ne!(reply.status, StatusCode::UNAUTHORIZED);

    let reply = send(&test.app, Method::GET, "/api/passkeys", &session, None).await;
    let list = reply.json.as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_ne!(list[0]["last_used_at"], Value::Null);
    let id = list[0]["id"].clone();

    // A challenge is answered once.
    let challenge = post(&test.app, "/api/session/passkey/options", &[], json!({})).await;
    let body = json!({
        "ceremony": challenge.json["ceremony"],
        "credential": passkey.sign_in(&challenge.json["options"]),
    });
    let reply = post(&test.app, "/api/session/passkey", &[], body.clone()).await;
    assert_eq!(reply.status, StatusCode::CREATED);
    let reply = post(&test.app, "/api/session/passkey", &[], body).await;
    assert_eq!(reply.status, StatusCode::CONFLICT);
    assert_eq!(reply.json["error"]["code"], "passkey_expired");

    // A replayed answer to a new challenge does not verify.
    let stale = passkey.sign_in(&challenge.json["options"]);
    let challenge = post(&test.app, "/api/session/passkey/options", &[], json!({})).await;
    let body = json!({ "ceremony": challenge.json["ceremony"], "credential": stale });
    let reply = post(&test.app, "/api/session/passkey", &[], body).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    assert_eq!(reply.json["error"]["code"], "passkey_invalid");

    // Another site cannot use the answer.
    passkey.origin = "https://books.example.com.evil.example".to_owned();
    let reply = passkey_sign_in(&test, &mut passkey).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    passkey.origin = PUBLIC_URL.to_owned();

    // An unknown passkey opens nothing.
    let mut unknown = SoftPasskey::new(PUBLIC_URL);
    let reply = post(&test.app, "/api/passkeys/options", &session, json!({})).await;
    unknown.register(&reply.json["options"]);
    let reply = passkey_sign_in(&test, &mut unknown).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);

    // Removed: it no longer signs in.
    let with_csrf = [session[0], CSRF];
    let path = format!("/api/passkeys/{id}");
    let reply = send(&test.app, Method::DELETE, &path, &with_csrf, None).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    let reply = passkey_sign_in(&test, &mut passkey).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);

    let kinds = events(&pool).await;
    for kind in ["passkey.added", "passkey.removed"] {
        assert!(kinds.contains(&kind.to_owned()), "{kind}");
    }
}

#[sqlx::test]
async fn passkeys_of_disabled_users_and_other_users_ceremonies_are_refused(pool: PgPool) {
    let (user_id, _) = users::invite(&pool, EMAIL, None).await.unwrap();
    users::invite(&pool, "ana@example.com", None).await.unwrap();
    let test = TestApp::signed_out(pool.clone());
    let cookie = test.signed_in_cookie(EMAIL).await;
    let mut passkey = register_passkey(&test, &cookie).await;

    // A registration started by one user cannot be finished by another.
    age_codes(&pool, 120).await;
    let ana = test.signed_in_cookie("ana@example.com").await;
    let challenge = post(
        &test.app,
        "/api/passkeys/options",
        &[(COOKIE.as_str(), cookie.as_str())],
        json!({}),
    )
    .await;
    let body = json!({
        "ceremony": challenge.json["ceremony"],
        "credential": SoftPasskey::new(PUBLIC_URL).register(&challenge.json["options"]),
    });
    let reply = post(
        &test.app,
        "/api/passkeys",
        &[(COOKIE.as_str(), ana.as_str())],
        body,
    )
    .await;
    assert_eq!(reply.status, StatusCode::CONFLICT);

    users::disable_user(&pool, user_id).await.unwrap();
    let reply = passkey_sign_in(&test, &mut passkey).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
}

/// Sends a real sign-in code through SMTP to a Mailpit server and reads it
/// back from Mailpit's API. Runs when `VERICOUNT_TEST_SMTP_URL` (e.g.
/// `smtp://127.0.0.1:1025`) and `VERICOUNT_TEST_MAILPIT_URL` (e.g.
/// `http://127.0.0.1:8025`) are set; skipped otherwise, except in CI, where
/// they are required.
#[sqlx::test]
async fn codes_go_through_smtp(pool: PgPool) {
    let (Ok(smtp_url), Ok(mailpit_url)) = (
        env::var("VERICOUNT_TEST_SMTP_URL"),
        env::var("VERICOUNT_TEST_MAILPIT_URL"),
    ) else {
        assert!(env::var_os("CI").is_none(), "Mailpit is required in CI");
        eprintln!("skipped: VERICOUNT_TEST_SMTP_URL and VERICOUNT_TEST_MAILPIT_URL are not set");
        return;
    };
    // A fresh address, so earlier runs against the same Mailpit do not count.
    let email = format!(
        "smtp-{}@example.com",
        sessions::new_token().get(..12).unwrap()
    );
    users::invite(&pool, &email, None).await.unwrap();
    let (mut state, _) = common::state_with_outbox(pool);
    state.mailer = Arc::new(Mailer::smtp(&smtp_url, "vericount <no-reply@example.com>").unwrap());
    let test = TestApp::new(state, Arc::default());
    assert_eq!(test.request_code(&email).await, StatusCode::ACCEPTED);

    let client = reqwest::Client::new();
    let search = format!("{mailpit_url}/api/v1/search");
    let mut message = None;
    for _ in 0..50 {
        let found: Value = client
            .get(&search)
            .query(&[("query", format!("to:{email}"))])
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .map(|body| serde_json::from_slice(&body).unwrap())
            .unwrap();
        if let Some(first) = found["messages"].as_array().and_then(|m| m.first()) {
            message = Some(first.clone());
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let message = message.expect("the code did not reach Mailpit");
    assert_eq!(message["From"]["Address"], "no-reply@example.com");
    let id = message["ID"].as_str().unwrap();
    let full: Value = client
        .get(format!("{mailpit_url}/api/v1/message/{id}"))
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .map(|body| serde_json::from_slice(&body).unwrap())
        .unwrap();
    let code = code_in(full["Text"].as_str().unwrap());
    assert!(full["Subject"].as_str().unwrap().contains(&code));
    assert!(full["Text"]
        .as_str()
        .unwrap()
        .contains("Caduca en 10 minutos"));

    let reply = test.sign_in(&email, &code).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.json);
}
