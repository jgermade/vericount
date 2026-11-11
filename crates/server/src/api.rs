//! JSON API.

use axum::{
    extract::{rejection::JsonRejection, Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::{Europe::Madrid, Tz};
use serde::{Deserialize, Serialize};
use vericounts_core::{
    invoice::{self, LineInput},
    qr,
};

use crate::{
    store::{self, ChainReport, InvoiceDetail, InvoiceSummary, NewInvoice, StoreError},
    AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/invoices", get(list_invoices).post(create_invoice))
        .route("/invoices/{id}", get(get_invoice))
        .route("/invoices/{id}/cancellation", post(cancel_invoice))
        .route("/chain/verification", get(verify_chain))
        .fallback(|| async { ApiError::new(StatusCode::NOT_FOUND, "not_found", "not found") })
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
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
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
        (self.status, Json(serde_json_error(&body))).into_response()
    }
}

#[derive(Serialize)]
struct ErrorEnvelope<'a> {
    error: &'a ErrorBody<'a>,
}

fn serde_json_error<'a>(body: &'a ErrorBody<'a>) -> ErrorEnvelope<'a> {
    ErrorEnvelope { error: body }
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
            StoreError::NotFound => {
                Self::new(StatusCode::NOT_FOUND, "not_found", "invoice not found")
            }
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
struct Recipient {
    name: String,
    nif: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateInvoice {
    series: String,
    issue_date: Option<NaiveDate>,
    recipient: Recipient,
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
    let detail = store::get_invoice(&state.pool, &state.config.issuer.nif, id).await?;
    let qr_url = qr::qr_url(
        state.config.aeat_environment,
        &detail.invoice.issuer_nif,
        &detail.invoice.series_number,
        detail.invoice.issue_date,
        detail.invoice.total_amount,
    );
    Ok(InvoiceResponse { detail, qr_url })
}

async fn create_invoice(
    State(state): State<AppState>,
    body: Result<Json<CreateInvoice>, JsonRejection>,
) -> Result<(StatusCode, Json<InvoiceResponse>), ApiError> {
    let Json(body) = body?;
    let new = NewInvoice {
        series: invoice::validate_series(&body.series)?,
        issue_date: body.issue_date,
        recipient_name: invoice::validate_text("recipient.name", &body.recipient.name)?,
        recipient_nif: invoice::validate_tax_id("recipient.nif", &body.recipient.nif)?,
        description: invoice::validate_text("description", &body.description)?,
        totals: invoice::compute_totals(&body.lines)?,
    };
    let id = store::create_invoice(&state.pool, &state.config.issuer, &new, now()).await?;
    Ok((
        StatusCode::CREATED,
        Json(invoice_response(&state, id).await?),
    ))
}

async fn list_invoices(
    State(state): State<AppState>,
) -> Result<Json<Vec<InvoiceSummary>>, ApiError> {
    Ok(Json(
        store::list_invoices(&state.pool, &state.config.issuer.nif).await?,
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
    Path(id): Path<i64>,
) -> Result<Json<InvoiceResponse>, ApiError> {
    store::cancel_invoice(&state.pool, &state.config.issuer, id, now()).await?;
    Ok(Json(invoice_response(&state, id).await?))
}

async fn verify_chain(State(state): State<AppState>) -> Result<Json<ChainReport>, ApiError> {
    Ok(Json(
        store::verify_chain(&state.pool, &state.config.issuer.nif).await?,
    ))
}
