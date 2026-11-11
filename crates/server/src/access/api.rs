//! HTTP routes of access: signing in and out (with a code or a passkey),
//! the user's passkeys, and the users who may sign in.

use axum::{
    extract::{rejection::JsonRejection, Path, State},
    http::{
        header::{SET_COOKIE, USER_AGENT},
        HeaderMap, StatusCode,
    },
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Extension, Json, Router,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use webauthn_rs::prelude::{PublicKeyCredential, RegisterPublicKeyCredential};

use crate::{
    access::{
        passkeys::{self, PasskeyError},
        sessions::{self, CurrentUser, LoginError},
        users,
    },
    api::{internal, ApiError},
    error::StoreError,
    events::log_event,
    mail, AppState,
};

/// Routes that work without a session: signing in.
pub fn public_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/session",
            get(current_session)
                .post(open_session)
                .delete(close_session),
        )
        .route("/session/code", post(request_code))
        .route("/session/passkey/options", post(passkey_sign_in_options))
        .route("/session/passkey", post(passkey_sign_in))
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/passkeys", get(list_passkeys).post(add_passkey))
        .route("/passkeys/options", post(passkey_registration_options))
        .route("/passkeys/{id}", delete(remove_passkey))
        .route("/users", get(list_users).post(invite_user))
        .route("/users/{id}", delete(disable_user))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CodeRequest {
    email: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CodeSignIn {
    email: String,
    code: String,
}

#[derive(Serialize)]
struct SessionResponse {
    email: String,
    /// How many passkeys the user has; the client offers one when zero.
    passkeys: i64,
    /// True when this sign-in was the user's sign-up.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    first_sign_in: bool,
}

fn user_agent(headers: &HeaderMap) -> &str {
    headers
        .get(USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
}

/// Sets the session cookie and answers with who is signed in.
async fn signed_in(state: &AppState, signed_in: sessions::SignedIn) -> Result<Response, ApiError> {
    let passkeys = passkeys::count(&state.pool, signed_in.user_id).await?;
    Ok((
        StatusCode::CREATED,
        [(SET_COOKIE, sessions::session_cookie(Some(&signed_in.token)))],
        Json(SessionResponse {
            email: signed_in.email,
            passkeys,
            first_sign_in: signed_in.first_sign_in,
        }),
    )
        .into_response())
}

/// Sends a sign-in code by email. The answer is the same whether the
/// address has an account or not.
async fn request_code(
    State(state): State<AppState>,
    body: Result<Json<CodeRequest>, JsonRejection>,
) -> Result<StatusCode, ApiError> {
    let Json(body) = body?;
    sessions::request_code(&state, &body.email).await?;
    Ok(StatusCode::ACCEPTED)
}

async fn open_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<CodeSignIn>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(body) = body?;
    match sessions::sign_in_with_code(&state, &body.email, &body.code, user_agent(&headers)).await {
        Ok(session) => signed_in(&state, session).await,
        Err(LoginError::InvalidCode) => Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "invalid_code",
            "the code is wrong or has expired",
        )),
        Err(LoginError::Internal) => Err(internal()),
    }
}

async fn current_session(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<SessionResponse>, ApiError> {
    match sessions::authenticate(&state.pool, &headers).await {
        Some(user) => Ok(Json(SessionResponse {
            passkeys: passkeys::count(&state.pool, user.id).await?,
            email: user.email,
            first_sign_in: false,
        })),
        None => Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "unauthenticated",
            "sign in to continue",
        )),
    }
}

async fn close_session(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(user) = sessions::authenticate(&state.pool, &headers).await {
        if let Err(error) = sessions::revoke_session(&state.pool, user.session_id).await {
            eprintln!("database error: {error}");
        }
        log_event(&state.pool, Some(user.id), "session.closed", "").await;
    }
    (
        StatusCode::NO_CONTENT,
        [(SET_COOKIE, sessions::session_cookie(None))],
    )
        .into_response()
}

impl From<PasskeyError> for ApiError {
    fn from(error: PasskeyError) -> Self {
        match error {
            PasskeyError::CeremonyExpired => Self::new(
                StatusCode::CONFLICT,
                "passkey_expired",
                "the passkey request expired, try again",
            ),
            PasskeyError::Invalid => Self::new(
                StatusCode::UNAUTHORIZED,
                "passkey_invalid",
                "the passkey was not accepted",
            ),
            PasskeyError::AlreadyRegistered => Self::new(
                StatusCode::CONFLICT,
                "passkey_exists",
                "this passkey is already registered",
            ),
            PasskeyError::NotFound => Self::new(StatusCode::NOT_FOUND, "not_found", "not found"),
            PasskeyError::Busy => Self::new(
                StatusCode::TOO_MANY_REQUESTS,
                "too_many_attempts",
                "too many pending sign-ins, try again later",
            ),
            PasskeyError::Internal => internal(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct PasskeySignIn {
    ceremony: Uuid,
    credential: PublicKeyCredential,
}

async fn passkey_sign_in_options(
    State(state): State<AppState>,
) -> Result<Json<passkeys::Challenge>, ApiError> {
    Ok(Json(passkeys::start_sign_in(&state).await?))
}

async fn passkey_sign_in(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<PasskeySignIn>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(body) = body?;
    let session = passkeys::finish_sign_in(
        &state,
        body.ceremony,
        &body.credential,
        user_agent(&headers),
    )
    .await?;
    signed_in(&state, session).await
}

#[derive(Debug, Deserialize)]
struct NewPasskey {
    ceremony: Uuid,
    #[serde(default)]
    name: String,
    credential: RegisterPublicKeyCredential,
}

async fn list_passkeys(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
) -> Result<Json<Vec<passkeys::PasskeyInfo>>, ApiError> {
    Ok(Json(passkeys::list(&state.pool, user.id).await?))
}

async fn passkey_registration_options(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
) -> Result<Json<passkeys::Challenge>, ApiError> {
    Ok(Json(passkeys::start_registration(&state, &user).await?))
}

async fn add_passkey(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    body: Result<Json<NewPasskey>, JsonRejection>,
) -> Result<(StatusCode, Json<passkeys::PasskeyInfo>), ApiError> {
    let Json(body) = body?;
    let info =
        passkeys::finish_registration(&state, &user, body.ceremony, &body.name, &body.credential)
            .await?;
    Ok((StatusCode::CREATED, Json(info)))
}

async fn remove_passkey(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    passkeys::remove(&state.pool, user.id, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Invitation {
    email: String,
}

async fn list_users(State(state): State<AppState>) -> Result<Json<Vec<users::UserInfo>>, ApiError> {
    Ok(Json(users::list_users(&state.pool).await?))
}

/// Lets someone sign up with their email address, and tells them so.
async fn invite_user(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    body: Result<Json<Invitation>, JsonRejection>,
) -> Result<StatusCode, ApiError> {
    let Json(body) = body?;
    let (_, email) = users::invite(&state.pool, &body.email, Some(user.id))
        .await
        .map_err(|error| match error {
            users::InviteError::InvalidEmail => ApiError {
                status: StatusCode::UNPROCESSABLE_ENTITY,
                code: "validation",
                message: "invalid email address".to_owned(),
                field: Some("email".to_owned()),
            },
            users::InviteError::AlreadyInvited => ApiError::new(
                StatusCode::CONFLICT,
                "user_exists",
                "this address can already sign in",
            ),
            users::InviteError::Database(error) => error.into(),
        })?;
    state.mailer.dispatch(mail::invitation(
        &email,
        &user.email,
        state.config.public_url.as_str(),
    ));
    Ok(StatusCode::CREATED)
}

async fn disable_user(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    if id == user.id {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "cannot_remove_yourself",
            "you cannot remove your own access",
        ));
    }
    if !users::disable_user(&state.pool, id).await? {
        return Err(StoreError::NotFound.into());
    }
    log_event(&state.pool, Some(user.id), "user.disabled", &id.to_string()).await;
    Ok(StatusCode::NO_CONTENT)
}
