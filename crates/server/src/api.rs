//! JSON API.

use axum::{
    extract::{rejection::JsonRejection, Path, Query, State},
    http::{
        header::{CACHE_CONTROL, CONTENT_TYPE, SET_COOKIE, USER_AGENT},
        HeaderMap, StatusCode,
    },
    middleware,
    response::{IntoResponse, Response},
    routing::{delete, get, post, put},
    Extension, Json, Router,
};
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use chrono_tz::{Europe::Madrid, Tz};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use vericount_core::{
    invoice::{self, Line, LineInput, TaxBreakdown},
    qr,
};
use webauthn_rs::prelude::{PublicKeyCredential, RegisterPublicKeyCredential};

use crate::{
    aeat_service::{self, CertificateError},
    auth::{self, CurrentUser, LoginError},
    certificate::CertificateInfo,
    directory::{self, Company, Contact, ContactInput, NewSeries, Series, SeriesUpdate},
    mail,
    passkeys::{self, PasskeyError},
    store::{self, ChainReport, InvoiceDetail, InvoiceSummary, NewInvoice, StoreError},
    submission::{self, LiveBlocker, QueueStatus, RunOutcome},
    AppState,
};

pub fn router(state: AppState) -> Router<AppState> {
    let protected = Router::new()
        .route("/invoices", get(list_invoices).post(create_invoice))
        .route("/invoices/preview", post(preview_invoice))
        .route("/invoices/{id}", get(get_invoice))
        .route("/invoices/{id}/cancellation", post(cancel_invoice))
        .route("/invoices/{id}/correction", post(correct_invoice))
        .route("/invoices/{id}/qr.svg", get(invoice_qr))
        .route("/summary", get(summary))
        .route("/chain/verification", get(verify_chain))
        .route("/company", get(get_company).put(put_company))
        .route("/series", get(list_series).post(create_series))
        .route("/series/{code}", put(update_series))
        .route("/contacts", get(list_contacts).post(create_contact))
        .route("/contacts/{id}", get(get_contact).put(update_contact))
        .route("/aeat", get(aeat_status))
        .route("/aeat/submissions", post(submit_now))
        .route(
            "/aeat/certificate",
            put(upload_certificate).delete(remove_certificate),
        )
        .route("/aeat/live", post(activate_live_mode))
        .route("/passkeys", get(list_passkeys).post(add_passkey))
        .route("/passkeys/options", post(passkey_registration_options))
        .route("/passkeys/{id}", delete(remove_passkey))
        .route("/users", get(list_users).post(invite_user))
        .route("/users/{id}", delete(disable_user))
        .fallback(|| async { ApiError::new(StatusCode::NOT_FOUND, "not_found", "not found") })
        .layer(middleware::from_fn_with_state(state, auth::require_session));
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
        .merge(protected)
        .layer(middleware::from_fn(auth::csrf))
}

/// Current time in Spain, which is what Veri*factu records carry.
fn now() -> DateTime<Tz> {
    Utc::now().with_timezone(&Madrid)
}

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
    field: Option<String>,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            field: None,
        }
    }
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    code: &'a str,
    message: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    field: Option<&'a str>,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = ErrorBody {
            code: self.code,
            message: &self.message,
            field: self.field.as_deref(),
        };
        (self.status, Json(ErrorEnvelope { error: &body })).into_response()
    }
}

#[derive(Serialize)]
struct ErrorEnvelope<'a> {
    error: &'a ErrorBody<'a>,
}

impl From<StoreError> for ApiError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::Validation(error) => Self {
                status: StatusCode::UNPROCESSABLE_ENTITY,
                code: "validation",
                message: error.message,
                field: Some(error.field),
            },
            StoreError::NotFound => Self::new(StatusCode::NOT_FOUND, "not_found", "not found"),
            StoreError::CompanyMissing => Self::new(
                StatusCode::CONFLICT,
                "company_missing",
                "enter the company data first",
            ),
            StoreError::Conflict(code, message) => Self::new(StatusCode::CONFLICT, code, message),
            StoreError::AlreadyCancelled => Self::new(
                StatusCode::CONFLICT,
                "already_cancelled",
                "the invoice is already cancelled",
            ),
            StoreError::Database(error) => {
                eprintln!("database error: {error}");
                Self::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal",
                    "internal error",
                )
            }
        }
    }
}

impl From<invoice::ValidationError> for ApiError {
    fn from(error: invoice::ValidationError) -> Self {
        StoreError::Validation(error).into()
    }
}

impl From<JsonRejection> for ApiError {
    fn from(rejection: JsonRejection) -> Self {
        Self::new(rejection.status(), "invalid_request", rejection.body_text())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateInvoice {
    series: String,
    issue_date: Option<NaiveDate>,
    contact_id: i64,
    description: String,
    lines: Vec<LineInput>,
}

#[derive(Serialize)]
struct InvoiceResponse {
    #[serde(flatten)]
    detail: InvoiceDetail,
    /// URL for the QR code printed on the invoice.
    qr_url: String,
}

async fn invoice_response(state: &AppState, id: i64) -> Result<InvoiceResponse, ApiError> {
    let detail =
        store::get_invoice(&state.pool, &directory::issuer(&state.pool).await?.nif, id).await?;
    // Practice invoices are not real: their QR code never points to the
    // production service.
    let environment = if detail.invoice.live {
        state.config.aeat_environment
    } else {
        qr::AeatEnvironment::Test
    };
    let qr_url = qr::qr_url(
        environment,
        &detail.invoice.issuer_nif,
        &detail.invoice.series_number,
        detail.invoice.issue_date,
        detail.invoice.total_amount,
    );
    Ok(InvoiceResponse { detail, qr_url })
}

impl CreateInvoice {
    fn validate(self) -> Result<NewInvoice, invoice::ValidationError> {
        Ok(NewInvoice {
            series: invoice::validate_series(&self.series)?,
            issue_date: self.issue_date,
            contact_id: self.contact_id,
            description: invoice::validate_text("description", &self.description)?,
            totals: invoice::compute_totals(&self.lines)?,
        })
    }
}

#[derive(Serialize)]
struct PreviewResponse {
    lines: Vec<Line>,
    taxes: Vec<TaxBreakdown>,
    #[serde(with = "rust_decimal::serde::str")]
    total_base: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    total_tax: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    total_amount: Decimal,
}

/// Validates an invoice and returns its totals without storing anything,
/// so the user can review them before issuing: issuing cannot be undone.
async fn preview_invoice(
    body: Result<Json<CreateInvoice>, JsonRejection>,
) -> Result<Json<PreviewResponse>, ApiError> {
    let Json(body) = body?;
    let totals = body.validate()?.totals;
    Ok(Json(PreviewResponse {
        lines: totals.lines,
        taxes: totals.taxes,
        total_base: totals.total_base,
        total_tax: totals.total_tax,
        total_amount: totals.total_amount,
    }))
}

async fn create_invoice(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    body: Result<Json<CreateInvoice>, JsonRejection>,
) -> Result<(StatusCode, Json<InvoiceResponse>), ApiError> {
    let Json(body) = body?;
    let new = body.validate()?;
    let id = store::create_invoice(
        &state.pool,
        &directory::issuer(&state.pool).await?,
        &new,
        now(),
    )
    .await?;
    auth::log_event(
        &state.pool,
        Some(user.id),
        "invoice.issued",
        &id.to_string(),
    )
    .await;
    Ok((
        StatusCode::CREATED,
        Json(invoice_response(&state, id).await?),
    ))
}

async fn list_invoices(
    State(state): State<AppState>,
) -> Result<Json<Vec<InvoiceSummary>>, ApiError> {
    Ok(Json(
        store::list_invoices(&state.pool, &directory::issuer(&state.pool).await?.nif).await?,
    ))
}

async fn get_invoice(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<InvoiceResponse>, ApiError> {
    Ok(Json(invoice_response(&state, id).await?))
}

async fn cancel_invoice(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path(id): Path<i64>,
) -> Result<Json<InvoiceResponse>, ApiError> {
    store::cancel_invoice(
        &state.pool,
        &directory::issuer(&state.pool).await?,
        id,
        now(),
    )
    .await?;
    auth::log_event(
        &state.pool,
        Some(user.id),
        "invoice.cancelled",
        &id.to_string(),
    )
    .await;
    Ok(Json(invoice_response(&state, id).await?))
}

async fn verify_chain(State(state): State<AppState>) -> Result<Json<ChainReport>, ApiError> {
    Ok(Json(
        store::verify_chain(&state.pool, &directory::issuer(&state.pool).await?.nif).await?,
    ))
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

fn internal() -> ApiError {
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal",
        "internal error",
    )
}

impl From<sqlx::Error> for ApiError {
    fn from(error: sqlx::Error) -> Self {
        StoreError::Database(error).into()
    }
}

/// Sets the session cookie and answers with who is signed in.
async fn signed_in(state: &AppState, signed_in: auth::SignedIn) -> Result<Response, ApiError> {
    let passkeys = passkeys::count(&state.pool, signed_in.user_id).await?;
    Ok((
        StatusCode::CREATED,
        [(SET_COOKIE, auth::session_cookie(Some(&signed_in.token)))],
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
    auth::request_code(&state, &body.email).await?;
    Ok(StatusCode::ACCEPTED)
}

async fn open_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<CodeSignIn>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(body) = body?;
    match auth::sign_in_with_code(&state, &body.email, &body.code, user_agent(&headers)).await {
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
    match auth::authenticate(&state.pool, &headers).await {
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
    if let Some(user) = auth::authenticate(&state.pool, &headers).await {
        if let Err(error) = auth::revoke_session(&state.pool, user.session_id).await {
            eprintln!("database error: {error}");
        }
        auth::log_event(&state.pool, Some(user.id), "session.closed", "").await;
    }
    (
        StatusCode::NO_CONTENT,
        [(SET_COOKIE, auth::session_cookie(None))],
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

async fn list_users(State(state): State<AppState>) -> Result<Json<Vec<auth::UserInfo>>, ApiError> {
    Ok(Json(auth::list_users(&state.pool).await?))
}

/// Lets someone sign up with their email address, and tells them so.
async fn invite_user(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    body: Result<Json<Invitation>, JsonRejection>,
) -> Result<StatusCode, ApiError> {
    let Json(body) = body?;
    let (_, email) = auth::invite(&state.pool, &body.email, Some(user.id))
        .await
        .map_err(|error| match error {
            auth::InviteError::InvalidEmail => ApiError {
                status: StatusCode::UNPROCESSABLE_ENTITY,
                code: "validation",
                message: "invalid email address".to_owned(),
                field: Some("email".to_owned()),
            },
            auth::InviteError::AlreadyInvited => ApiError::new(
                StatusCode::CONFLICT,
                "user_exists",
                "this address can already sign in",
            ),
            auth::InviteError::Database(error) => error.into(),
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
    if !auth::disable_user(&state.pool, id).await? {
        return Err(StoreError::NotFound.into());
    }
    auth::log_event(&state.pool, Some(user.id), "user.disabled", &id.to_string()).await;
    Ok(StatusCode::NO_CONTENT)
}

async fn aeat_status(State(state): State<AppState>) -> Result<Json<QueueStatus>, ApiError> {
    Ok(Json(
        submission::queue_status(
            &state.pool,
            &directory::issuer(&state.pool).await?.nif,
            &state.aeat,
        )
        .await?,
    ))
}

/// Sends the pending records now instead of waiting for the next run.
async fn submit_now(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
) -> Result<Json<RunOutcome>, ApiError> {
    let Some(submitter) = state.aeat.current() else {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "aeat_disabled",
            "records are not being sent",
        ));
    };
    let outcome = submission::run_once(
        &state.pool,
        &directory::issuer(&state.pool).await?,
        &submitter,
    )
    .await?;
    auth::log_event(
        &state.pool,
        Some(user.id),
        "aeat.submission_requested",
        &format!("{outcome:?}"),
    )
    .await;
    Ok(Json(outcome))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CertificateUpload {
    /// The file, in base64: PKCS#12 (.p12, .pfx) or PEM.
    file: String,
    /// Password of a PKCS#12 file.
    #[serde(default)]
    password: String,
}

/// Reloads the submitter after a change of mode or certificate.
async fn reload_aeat(state: &AppState) {
    if let Err(error) = state.aeat.reload(&state.pool).await {
        eprintln!("records are not being sent: {error}");
    }
}

async fn upload_certificate(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    body: Result<Json<CertificateUpload>, JsonRejection>,
) -> Result<Json<CertificateInfo>, ApiError> {
    use base64::Engine;
    let Json(body) = body?;
    let file = base64::engine::general_purpose::STANDARD
        .decode(body.file.trim())
        .map_err(|_| {
            ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "certificate_unreadable",
                "the file is not valid base64",
            )
        })?;
    let info = aeat_service::save_certificate(
        &state.pool,
        state.aeat.settings.secret_key.as_ref(),
        user.id,
        &file,
        &body.password,
        Utc::now(),
    )
    .await
    .map_err(|error| match error {
        CertificateError::SecretKeyMissing => ApiError::new(
            StatusCode::CONFLICT,
            "secret_key_missing",
            "VERICOUNT_SECRET_KEY is not set: certificates cannot be stored",
        ),
        CertificateError::Unreadable(reason) => ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "certificate_unreadable",
            reason,
        ),
        CertificateError::Expired => ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "certificate_expired",
            "the certificate has expired",
        ),
        CertificateError::Database(error) => StoreError::Database(error).into(),
    })?;
    auth::log_event(
        &state.pool,
        Some(user.id),
        "aeat.certificate_uploaded",
        &format!("{} {}", info.subject, info.nif.as_deref().unwrap_or("")),
    )
    .await;
    reload_aeat(&state).await;
    Ok(Json(info))
}

async fn remove_certificate(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
) -> Result<StatusCode, ApiError> {
    if !aeat_service::remove_certificate(&state.pool).await? {
        return Err(StoreError::NotFound.into());
    }
    auth::log_event(&state.pool, Some(user.id), "aeat.certificate_removed", "").await;
    reload_aeat(&state).await;
    Ok(StatusCode::NO_CONTENT)
}

/// Turns live mode on: from now on invoices are real and their records go
/// to the AEAT. Practice invoices are no longer shown. There is no way
/// back.
async fn activate_live_mode(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
) -> Result<Json<QueueStatus>, ApiError> {
    let issuer = directory::issuer(&state.pool).await?;
    let status = submission::queue_status(&state.pool, &issuer.nif, &state.aeat).await?;
    if status.mode == "live" {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "already_live",
            "live mode is already on",
        ));
    }
    if let Some(blocker) = status.live_blockers.first() {
        let (code, message) = match blocker {
            LiveBlocker::CertificateMissing => {
                ("certificate_missing", "upload a certificate first")
            }
            LiveBlocker::CertificateExpired => {
                ("certificate_expired", "the certificate has expired")
            }
            LiveBlocker::ProducerMissing => (
                "producer_missing",
                "set VERICOUNT_SOFTWARE_PRODUCER_NAME and VERICOUNT_SOFTWARE_PRODUCER_NIF",
            ),
        };
        return Err(ApiError::new(StatusCode::CONFLICT, code, message));
    }
    store::activate_live_mode(&state.pool, &issuer.nif, user.id).await?;
    auth::log_event(
        &state.pool,
        Some(user.id),
        "aeat.live_mode_activated",
        &issuer.nif,
    )
    .await;
    reload_aeat(&state).await;
    Ok(Json(
        submission::queue_status(&state.pool, &issuer.nif, &state.aeat).await?,
    ))
}

async fn get_company(State(state): State<AppState>) -> Result<Json<Company>, ApiError> {
    directory::company(&state.pool)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::from(StoreError::CompanyMissing))
}

async fn put_company(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    body: Result<Json<Company>, JsonRejection>,
) -> Result<Json<Company>, ApiError> {
    let Json(body) = body?;
    let company = directory::save_company(&state.pool, body).await?;
    auth::log_event(&state.pool, Some(user.id), "company.saved", &company.nif).await;
    Ok(Json(company))
}

async fn list_series(State(state): State<AppState>) -> Result<Json<Vec<Series>>, ApiError> {
    Ok(Json(directory::list_series(&state.pool).await?))
}

async fn create_series(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    body: Result<Json<NewSeries>, JsonRejection>,
) -> Result<(StatusCode, Json<Series>), ApiError> {
    let Json(body) = body?;
    let series = directory::create_series(&state.pool, body).await?;
    auth::log_event(&state.pool, Some(user.id), "series.created", &series.code).await;
    Ok((StatusCode::CREATED, Json(series)))
}

async fn update_series(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path(code): Path<String>,
    body: Result<Json<SeriesUpdate>, JsonRejection>,
) -> Result<Json<Series>, ApiError> {
    let Json(body) = body?;
    let series = directory::update_series(&state.pool, &code, body).await?;
    auth::log_event(&state.pool, Some(user.id), "series.updated", &series.code).await;
    Ok(Json(series))
}

async fn list_contacts(State(state): State<AppState>) -> Result<Json<Vec<Contact>>, ApiError> {
    Ok(Json(directory::list_contacts(&state.pool).await?))
}

async fn get_contact(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<Contact>, ApiError> {
    Ok(Json(directory::get_contact(&state.pool, id).await?))
}

async fn create_contact(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    body: Result<Json<ContactInput>, JsonRejection>,
) -> Result<(StatusCode, Json<Contact>), ApiError> {
    let Json(body) = body?;
    let contact = directory::create_contact(&state.pool, body).await?;
    let id = contact.id.to_string();
    auth::log_event(&state.pool, Some(user.id), "contact.created", &id).await;
    Ok((StatusCode::CREATED, Json(contact)))
}

async fn update_contact(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path(id): Path<i64>,
    body: Result<Json<ContactInput>, JsonRejection>,
) -> Result<Json<Contact>, ApiError> {
    let Json(body) = body?;
    let contact = directory::update_contact(&state.pool, id, body).await?;
    auth::log_event(
        &state.pool,
        Some(user.id),
        "contact.updated",
        &id.to_string(),
    )
    .await;
    Ok(Json(contact))
}

/// QR code of an invoice as an SVG image. It encodes the AEAT validation
/// URL, with error correction level M as the Veri*factu rules require.
async fn invoice_qr(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Response, ApiError> {
    let url = invoice_response(&state, id).await?.qr_url;
    let code = qrcode::QrCode::with_error_correction_level(url.as_bytes(), qrcode::EcLevel::M)
        .map_err(|error| {
            eprintln!("cannot build QR code: {error}");
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "internal error",
            )
        })?;
    let svg = code
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(240, 240)
        .quiet_zone(true)
        .build();
    Ok((
        [
            (CONTENT_TYPE, "image/svg+xml"),
            (CACHE_CONTROL, "private, max-age=86400"),
        ],
        svg,
    )
        .into_response())
}

#[derive(Debug, Deserialize)]
struct SummaryQuery {
    year: Option<i32>,
    quarter: Option<u32>,
}

/// Totals of a quarter; the current one by default.
async fn summary(
    State(state): State<AppState>,
    Query(query): Query<SummaryQuery>,
) -> Result<Json<store::PeriodSummary>, ApiError> {
    let today = now().date_naive();
    let year = query.year.unwrap_or(today.year());
    let quarter = query.quarter.unwrap_or(today.month0() / 3 + 1);
    let issuer = directory::issuer(&state.pool).await?;
    Ok(Json(
        store::period_summary(&state.pool, &issuer.nif, year, quarter).await?,
    ))
}

/// Replaces the invoice's rejected record with a new one, to be sent
/// again ("subsanación").
async fn correct_invoice(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path(id): Path<i64>,
) -> Result<Json<InvoiceResponse>, ApiError> {
    let issuer = directory::issuer(&state.pool).await?;
    store::correct_rejected_record(&state.pool, &issuer, id, now()).await?;
    let detail = id.to_string();
    auth::log_event(
        &state.pool,
        Some(user.id),
        "invoice.record_corrected",
        &detail,
    )
    .await;
    Ok(Json(invoice_response(&state, id).await?))
}
