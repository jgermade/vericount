//! HTTP routes of the invoices: issuing, listing, cancelling and
//! correcting them, their QR code, the chain check and the quarter's totals.

use axum::{
    extract::{rejection::JsonRejection, Path, Query, State},
    http::{
        header::{CACHE_CONTROL, CONTENT_TYPE},
        StatusCode,
    },
    response::{IntoResponse, Response},
    routing::{get, post},
    Extension, Json, Router,
};
use chrono::{Datelike, NaiveDate};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use vericount_core::{
    invoice::{self, Line, LineInput, TaxBreakdown},
    qr,
};

use crate::{
    access::sessions::CurrentUser,
    api::{now, ApiError},
    company,
    events::log_event,
    invoices::store::{self, ChainReport, InvoiceDetail, InvoiceSummary, NewInvoice},
    AppState,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/invoices", get(list_invoices).post(create_invoice))
        .route("/invoices/preview", post(preview_invoice))
        .route("/invoices/{id}", get(get_invoice))
        .route("/invoices/{id}/cancellation", post(cancel_invoice))
        .route("/invoices/{id}/correction", post(correct_invoice))
        .route("/invoices/{id}/qr.svg", get(invoice_qr))
        .route("/summary", get(summary))
        .route("/chain/verification", get(verify_chain))
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
    let detail = store::get_invoice(
        &state.pool,
        &company::store::issuer(&state.pool).await?.nif,
        id,
    )
    .await?;
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
        &company::store::issuer(&state.pool).await?,
        &new,
        now(),
    )
    .await?;
    log_event(
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
        store::list_invoices(&state.pool, &company::store::issuer(&state.pool).await?.nif).await?,
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
        &company::store::issuer(&state.pool).await?,
        id,
        now(),
    )
    .await?;
    log_event(
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
        store::verify_chain(&state.pool, &company::store::issuer(&state.pool).await?.nif).await?,
    ))
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
    let issuer = company::store::issuer(&state.pool).await?;
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
    let issuer = company::store::issuer(&state.pool).await?;
    store::correct_rejected_record(&state.pool, &issuer, id, now()).await?;
    let detail = id.to_string();
    log_event(
        &state.pool,
        Some(user.id),
        "invoice.record_corrected",
        &detail,
    )
    .await;
    Ok(Json(invoice_response(&state, id).await?))
}
