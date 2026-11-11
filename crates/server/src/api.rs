//! JSON API.

use axum::{
    extract::{rejection::JsonRejection, Path, Query, State},
    http::{
        header::{CACHE_CONTROL, CONTENT_TYPE, SET_COOKIE, USER_AGENT},
        HeaderMap, StatusCode,
    },
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post, put},
    Extension, Json, Router,
};
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use chrono_tz::{Europe::Madrid, Tz};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use vericounts_core::{
    invoice::{self, Line, LineInput, TaxBreakdown},
    qr,
};

use crate::{
    auth::{self, CurrentUser, LoginError},
    directory::{self, Company, Contact, ContactInput, NewSeries, Series, SeriesUpdate},
    store::{self, ChainReport, InvoiceDetail, InvoiceSummary, NewInvoice, StoreError},
    submission::{self, QueueStatus, RunOutcome},
    AppState,
};

pub fn router(state: AppState) -> Router<AppState> {
    let protected = Router::new()
        .route("/invoices", get(list_invoices).post(create_invoice))
        .route("/invoices/preview", post(preview_invoice))
        .route("/invoices/{id}", get(get_invoice))
        .route("/invoices/{id}/cancellation", post(cancel_invoice))
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
        .fallback(|| async { ApiError::new(StatusCode::NOT_FOUND, "not_found", "not found") })
        .layer(middleware::from_fn_with_state(state, auth::require_session));
    Router::new()
        .route(
            "/session",
            get(current_session)
                .post(open_session)
                .delete(close_session),
        )
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
    let qr_url = qr::qr_url(
        state.config.aeat_environment,
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
struct Credentials {
    username: String,
    password: String,
}

#[derive(Serialize)]
struct SessionResponse {
    username: String,
}

async fn open_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Credentials>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(credentials) = body?;
    let user_agent = headers
        .get(USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    match auth::login(
        &state,
        &credentials.username,
        &credentials.password,
        user_agent,
    )
    .await
    {
        Ok(token) => Ok((
            StatusCode::CREATED,
            [(SET_COOKIE, auth::session_cookie(Some(&token)))],
            Json(SessionResponse {
                username: credentials.username.trim().to_lowercase(),
            }),
        )
            .into_response()),
        Err(LoginError::InvalidCredentials) => Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "invalid_credentials",
            "wrong username or password",
        )),
        Err(LoginError::TooManyAttempts) => Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "too_many_attempts",
            "too many failed attempts, try again in a few minutes",
        )),
        Err(LoginError::Internal) => Err(ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "internal error",
        )),
    }
}

async fn current_session(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<SessionResponse>, ApiError> {
    match auth::authenticate(&state.pool, &headers).await {
        Some(user) => Ok(Json(SessionResponse {
            username: user.username,
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

async fn aeat_status(State(state): State<AppState>) -> Result<Json<QueueStatus>, ApiError> {
    Ok(Json(
        submission::queue_status(
            &state.pool,
            &directory::issuer(&state.pool).await?.nif,
            state.submitter.is_some(),
        )
        .await?,
    ))
}

/// Sends the pending records now instead of waiting for the next run.
async fn submit_now(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
) -> Result<Json<RunOutcome>, ApiError> {
    let Some(submitter) = &state.submitter else {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "aeat_disabled",
            "sending records to the AEAT is not enabled",
        ));
    };
    let outcome = submission::run_once(
        &state.pool,
        &directory::issuer(&state.pool).await?,
        submitter,
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
