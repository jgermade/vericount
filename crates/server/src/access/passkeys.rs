//! Passkeys (WebAuthn): registering them and signing in with them.
//!
//! A signed-in user registers a passkey; later, the browser offers it on
//! the sign-in screen without typing anything (a discoverable credential).
//! Each ceremony takes two requests: the first one stores the challenge in
//! `webauthn_ceremonies` and the second one consumes it, so a challenge
//! can only be answered once.

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use sqlx::{types::Json, PgPool};
use uuid::Uuid;
use webauthn_rs::prelude::{
    DiscoverableAuthentication, Passkey, PasskeyRegistration, PublicKeyCredential,
    RegisterPublicKeyCredential,
};

use crate::{
    access::sessions::{self, CurrentUser, SignedIn},
    events::log_event,
    AppState,
};

/// How long the browser has to answer a challenge.
const CEREMONY_MINUTES: i32 = 5;
/// Pending sign-in ceremonies allowed at once. They are opened without a
/// session, so their number is bounded.
const MAX_PENDING_SIGN_INS: i64 = 1000;

#[derive(Debug)]
pub enum PasskeyError {
    /// The challenge is unknown, already used or expired.
    CeremonyExpired,
    /// The browser's answer does not verify, or the passkey is unknown.
    Invalid,
    AlreadyRegistered,
    NotFound,
    Busy,
    Internal,
}

impl From<sqlx::Error> for PasskeyError {
    fn from(error: sqlx::Error) -> Self {
        eprintln!("database error: {error}");
        Self::Internal
    }
}

impl From<sessions::LoginError> for PasskeyError {
    fn from(_: sessions::LoginError) -> Self {
        Self::Internal
    }
}

/// A challenge for the browser: `options` goes to `navigator.credentials`
/// and `ceremony` comes back with the answer.
#[derive(Debug, Serialize)]
pub struct Challenge {
    pub ceremony: Uuid,
    pub options: Value,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct PasskeyInfo {
    pub id: i64,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

async fn store_ceremony(
    pool: &PgPool,
    kind: &str,
    user_id: Option<i64>,
    state: Value,
) -> Result<Uuid, sqlx::Error> {
    sqlx::query("DELETE FROM webauthn_ceremonies WHERE expires_at <= now()")
        .execute(pool)
        .await?;
    sqlx::query_scalar(
        "INSERT INTO webauthn_ceremonies (kind, user_id, state, expires_at)
         VALUES ($1, $2, $3, now() + make_interval(mins => $4))
         RETURNING id",
    )
    .bind(kind)
    .bind(user_id)
    .bind(state)
    .bind(CEREMONY_MINUTES)
    .fetch_one(pool)
    .await
}

/// Removes a ceremony and returns its state, if it is still valid.
async fn take_ceremony(
    pool: &PgPool,
    id: Uuid,
    kind: &str,
    user_id: Option<i64>,
) -> Result<Value, PasskeyError> {
    let state: Option<Value> = sqlx::query_scalar(
        "DELETE FROM webauthn_ceremonies
         WHERE id = $1 AND kind = $2 AND user_id IS NOT DISTINCT FROM $3 AND expires_at > now()
         RETURNING state",
    )
    .bind(id)
    .bind(kind)
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    state.ok_or(PasskeyError::CeremonyExpired)
}

fn to_value(value: &impl Serialize) -> Result<Value, PasskeyError> {
    serde_json::to_value(value).map_err(|_| PasskeyError::Internal)
}

/// Starts registering a passkey for the signed-in user.
pub async fn start_registration(
    state: &AppState,
    user: &CurrentUser,
) -> Result<Challenge, PasskeyError> {
    let handle: Uuid = sqlx::query_scalar("SELECT webauthn_id FROM users WHERE id = $1")
        .bind(user.id)
        .fetch_one(&state.pool)
        .await?;
    let existing: Vec<Json<Passkey>> =
        sqlx::query_scalar("SELECT credential FROM passkeys WHERE user_id = $1")
            .bind(user.id)
            .fetch_all(&state.pool)
            .await?;
    let exclude = existing.iter().map(|key| key.cred_id().clone()).collect();
    let (options, registration) = state
        .webauthn
        .start_passkey_registration(handle, &user.email, &user.email, Some(exclude))
        .map_err(|error| {
            eprintln!("cannot start passkey registration: {error}");
            PasskeyError::Internal
        })?;
    let mut options = to_value(&options)?;
    // Ask for a discoverable credential, so signing in needs no email.
    if let Some(selection) = options.pointer_mut("/publicKey/authenticatorSelection") {
        selection["residentKey"] = "required".into();
        selection["requireResidentKey"] = true.into();
    }
    let ceremony = store_ceremony(
        &state.pool,
        "registration",
        Some(user.id),
        to_value(&registration)?,
    )
    .await?;
    Ok(Challenge { ceremony, options })
}

/// Checks the browser's answer and stores the new passkey.
pub async fn finish_registration(
    state: &AppState,
    user: &CurrentUser,
    ceremony: Uuid,
    name: &str,
    credential: &RegisterPublicKeyCredential,
) -> Result<PasskeyInfo, PasskeyError> {
    let registration: PasskeyRegistration = serde_json::from_value(
        take_ceremony(&state.pool, ceremony, "registration", Some(user.id)).await?,
    )
    .map_err(|_| PasskeyError::Internal)?;
    let passkey = state
        .webauthn
        .finish_passkey_registration(credential, &registration)
        .map_err(|_| PasskeyError::Invalid)?;
    let name: String = name.trim().chars().take(60).collect();
    let info: Option<PasskeyInfo> = sqlx::query_as(
        "INSERT INTO passkeys (user_id, credential_id, credential, name)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (credential_id) DO NOTHING
         RETURNING id, name, created_at, last_used_at",
    )
    .bind(user.id)
    .bind(AsRef::<[u8]>::as_ref(passkey.cred_id()))
    .bind(Json(&passkey))
    .bind(&name)
    .fetch_optional(&state.pool)
    .await?;
    let info = info.ok_or(PasskeyError::AlreadyRegistered)?;
    log_event(
        &state.pool,
        Some(user.id),
        "passkey.added",
        &info.id.to_string(),
    )
    .await;
    Ok(info)
}

pub async fn list(pool: &PgPool, user_id: i64) -> Result<Vec<PasskeyInfo>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, name, created_at, last_used_at FROM passkeys
         WHERE user_id = $1 ORDER BY created_at, id",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
}

pub async fn count(pool: &PgPool, user_id: i64) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT count(*) FROM passkeys WHERE user_id = $1")
        .bind(user_id)
        .fetch_one(pool)
        .await
}

/// Removes one of the user's passkeys. The authenticator keeps it, but it
/// no longer opens a session.
pub async fn remove(pool: &PgPool, user_id: i64, id: i64) -> Result<(), PasskeyError> {
    let removed = sqlx::query("DELETE FROM passkeys WHERE id = $1 AND user_id = $2")
        .bind(id)
        .bind(user_id)
        .execute(pool)
        .await?
        .rows_affected()
        == 1;
    if !removed {
        return Err(PasskeyError::NotFound);
    }
    log_event(pool, Some(user_id), "passkey.removed", &id.to_string()).await;
    Ok(())
}

/// Starts a sign-in with any passkey of this site.
pub async fn start_sign_in(state: &AppState) -> Result<Challenge, PasskeyError> {
    let pending: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM webauthn_ceremonies
         WHERE kind = 'authentication' AND expires_at > now()",
    )
    .fetch_one(&state.pool)
    .await?;
    if pending >= MAX_PENDING_SIGN_INS {
        return Err(PasskeyError::Busy);
    }
    let (options, authentication) =
        state
            .webauthn
            .start_discoverable_authentication()
            .map_err(|error| {
                eprintln!("cannot start passkey sign-in: {error}");
                PasskeyError::Internal
            })?;
    let mut options = to_value(&options)?;
    // The client chooses between the autofill and the modal prompt.
    if let Some(options) = options.as_object_mut() {
        options.remove("mediation");
    }
    let ceremony = store_ceremony(
        &state.pool,
        "authentication",
        None,
        to_value(&authentication)?,
    )
    .await?;
    Ok(Challenge { ceremony, options })
}

/// Checks the browser's answer and opens a session for the passkey's user.
pub async fn finish_sign_in(
    state: &AppState,
    ceremony: Uuid,
    credential: &PublicKeyCredential,
    user_agent: &str,
) -> Result<SignedIn, PasskeyError> {
    let authentication: DiscoverableAuthentication =
        serde_json::from_value(take_ceremony(&state.pool, ceremony, "authentication", None).await?)
            .map_err(|_| PasskeyError::Internal)?;
    let (handle, credential_id) = state
        .webauthn
        .identify_discoverable_authentication(credential)
        .map_err(|_| PasskeyError::Invalid)?;
    let found: Option<(i64, i64, String, Json<Passkey>)> = sqlx::query_as(
        "SELECT p.id, u.id, u.email, p.credential
         FROM passkeys p JOIN users u ON u.id = p.user_id
         WHERE u.webauthn_id = $1 AND p.credential_id = $2
           AND u.disabled_at IS NULL AND u.email IS NOT NULL",
    )
    .bind(handle)
    .bind(credential_id)
    .fetch_optional(&state.pool)
    .await?;
    let Some((passkey_id, user_id, email, Json(mut passkey))) = found else {
        return Err(PasskeyError::Invalid);
    };
    let result = state
        .webauthn
        .finish_discoverable_authentication_v2(credential, authentication, &passkey)
        .map_err(|_| PasskeyError::Invalid)?;
    if !result.user_verified() {
        return Err(PasskeyError::Invalid);
    }
    // webauthn-rs has already refused a signature counter that did not go
    // up (a sign of a cloned authenticator); keep the new one.
    passkey.update_credential(&result);
    sqlx::query("UPDATE passkeys SET credential = $2, last_used_at = now() WHERE id = $1")
        .bind(passkey_id)
        .bind(Json(&passkey))
        .execute(&state.pool)
        .await?;
    Ok(sessions::open_session(&state.pool, user_id, email, user_agent, "passkey").await?)
}
