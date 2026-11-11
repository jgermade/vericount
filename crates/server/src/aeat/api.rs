//! HTTP routes of the AEAT sending: the queue's state, sending now, the
//! certificate, and turning live mode on.

use axum::{
    extract::{rejection::JsonRejection, State},
    http::StatusCode,
    routing::{get, post, put},
    Extension, Json, Router,
};
use chrono::Utc;
use serde::Deserialize;

use crate::{
    access::sessions::CurrentUser,
    aeat::{
        certificate::CertificateInfo,
        service::{self, CertificateError},
        submission::{self, LiveBlocker, QueueStatus, RunOutcome},
    },
    api::ApiError,
    company,
    error::StoreError,
    events::log_event,
    invoices::store,
    AppState,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/aeat", get(aeat_status))
        .route("/aeat/submissions", post(submit_now))
        .route(
            "/aeat/certificate",
            put(upload_certificate).delete(remove_certificate),
        )
        .route("/aeat/live", post(activate_live_mode))
}

async fn aeat_status(State(state): State<AppState>) -> Result<Json<QueueStatus>, ApiError> {
    Ok(Json(
        submission::queue_status(
            &state.pool,
            &company::store::issuer(&state.pool).await?.nif,
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
        &company::store::issuer(&state.pool).await?,
        &submitter,
    )
    .await?;
    log_event(
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
    let info = service::save_certificate(
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
    log_event(
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
    if !service::remove_certificate(&state.pool).await? {
        return Err(StoreError::NotFound.into());
    }
    log_event(&state.pool, Some(user.id), "aeat.certificate_removed", "").await;
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
    let issuer = company::store::issuer(&state.pool).await?;
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
    log_event(
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
