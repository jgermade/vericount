//! JSON API: the router that puts the domains' routes together, and the
//! error every route answers with. Each domain's routes are in its own
//! `api.rs`.

use axum::{
    extract::rejection::JsonRejection,
    http::StatusCode,
    middleware,
    response::{IntoResponse, Response},
    Json, Router,
};
use chrono::{DateTime, Utc};
use chrono_tz::{Europe::Madrid, Tz};
use serde::Serialize;
use vericount_core::invoice;

use crate::{access, aeat, company, contacts, error::StoreError, invoices, AppState};

pub fn router(state: AppState) -> Router<AppState> {
    let protected = Router::new()
        .merge(invoices::api::routes())
        .merge(company::api::routes())
        .merge(contacts::api::routes())
        .merge(aeat::api::routes())
        .merge(access::api::routes())
        .fallback(|| async { ApiError::new(StatusCode::NOT_FOUND, "not_found", "not found") })
        .layer(middleware::from_fn_with_state(
            state,
            access::sessions::require_session,
        ));
    Router::new()
        .merge(access::api::public_routes())
        .merge(protected)
        .layer(middleware::from_fn(access::sessions::csrf))
}

/// Current time in Spain, which is what Veri*factu records carry.
pub(crate) fn now() -> DateTime<Tz> {
    Utc::now().with_timezone(&Madrid)
}

#[derive(Debug)]
pub struct ApiError {
    pub(crate) status: StatusCode,
    pub(crate) code: &'static str,
    pub(crate) message: String,
    pub(crate) field: Option<String>,
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

pub(crate) fn internal() -> ApiError {
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
