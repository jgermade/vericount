//! HTTP routes of the contacts.

use axum::{
    extract::{rejection::JsonRejection, Path, State},
    http::StatusCode,
    routing::get,
    Extension, Json, Router,
};

use crate::{
    access::sessions::CurrentUser,
    api::ApiError,
    contacts::store::{self, Contact, ContactInput},
    events::log_event,
    AppState,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/contacts", get(list_contacts).post(create_contact))
        .route("/contacts/{id}", get(get_contact).put(update_contact))
}

async fn list_contacts(State(state): State<AppState>) -> Result<Json<Vec<Contact>>, ApiError> {
    Ok(Json(store::list_contacts(&state.pool).await?))
}

async fn get_contact(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<Contact>, ApiError> {
    Ok(Json(store::get_contact(&state.pool, id).await?))
}

async fn create_contact(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    body: Result<Json<ContactInput>, JsonRejection>,
) -> Result<(StatusCode, Json<Contact>), ApiError> {
    let Json(body) = body?;
    let contact = store::create_contact(&state.pool, body).await?;
    let id = contact.id.to_string();
    log_event(&state.pool, Some(user.id), "contact.created", &id).await;
    Ok((StatusCode::CREATED, Json(contact)))
}

async fn update_contact(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path(id): Path<i64>,
    body: Result<Json<ContactInput>, JsonRejection>,
) -> Result<Json<Contact>, ApiError> {
    let Json(body) = body?;
    let contact = store::update_contact(&state.pool, id, body).await?;
    log_event(
        &state.pool,
        Some(user.id),
        "contact.updated",
        &id.to_string(),
    )
    .await;
    Ok(Json(contact))
}
