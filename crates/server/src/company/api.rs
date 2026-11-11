//! HTTP routes of the company: its data and its numbering series.

use axum::{
    extract::{rejection::JsonRejection, Path, State},
    http::StatusCode,
    routing::{get, put},
    Extension, Json, Router,
};

use crate::{
    access::sessions::CurrentUser,
    api::ApiError,
    company::store::{self, Company, NewSeries, Series, SeriesUpdate},
    error::StoreError,
    events::log_event,
    AppState,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/company", get(get_company).put(put_company))
        .route("/series", get(list_series).post(create_series))
        .route("/series/{code}", put(update_series))
}

async fn get_company(State(state): State<AppState>) -> Result<Json<Company>, ApiError> {
    store::company(&state.pool)
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
    let company = store::save_company(&state.pool, body).await?;
    log_event(&state.pool, Some(user.id), "company.saved", &company.nif).await;
    Ok(Json(company))
}

async fn list_series(State(state): State<AppState>) -> Result<Json<Vec<Series>>, ApiError> {
    Ok(Json(store::list_series(&state.pool).await?))
}

async fn create_series(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    body: Result<Json<NewSeries>, JsonRejection>,
) -> Result<(StatusCode, Json<Series>), ApiError> {
    let Json(body) = body?;
    let series = store::create_series(&state.pool, body).await?;
    log_event(&state.pool, Some(user.id), "series.created", &series.code).await;
    Ok((StatusCode::CREATED, Json(series)))
}

async fn update_series(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path(code): Path<String>,
    body: Result<Json<SeriesUpdate>, JsonRejection>,
) -> Result<Json<Series>, ApiError> {
    let Json(body) = body?;
    let series = store::update_series(&state.pool, &code, body).await?;
    log_event(&state.pool, Some(user.id), "series.updated", &series.code).await;
    Ok(Json(series))
}
