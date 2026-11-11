//! Users: invitations, the list in Settings, and removing access. Who can
//! sign in is decided here; how they sign in, in `sessions` and `passkeys`.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;

use crate::{access::sessions::normalize_email, events::log_event};

#[derive(Debug)]
pub enum InviteError {
    InvalidEmail,
    AlreadyInvited,
    Database(sqlx::Error),
}

impl From<sqlx::Error> for InviteError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error)
    }
}

/// Lets an email address sign up. A disabled user with that address is
/// enabled again. Returns the user id and the normalised address.
pub async fn invite(
    pool: &PgPool,
    email: &str,
    invited_by: Option<i64>,
) -> Result<(i64, String), InviteError> {
    let email = normalize_email(email).ok_or(InviteError::InvalidEmail)?;
    let id: Option<i64> = sqlx::query_scalar(
        "INSERT INTO users (email, invited_by) VALUES ($1, $2)
         ON CONFLICT (email) DO UPDATE SET disabled_at = NULL, invited_by = $2
             WHERE users.disabled_at IS NOT NULL
         RETURNING id",
    )
    .bind(&email)
    .bind(invited_by)
    .fetch_optional(pool)
    .await?;
    let id = id.ok_or(InviteError::AlreadyInvited)?;
    log_event(pool, invited_by.or(Some(id)), "user.invited", &email).await;
    Ok((id, email))
}

/// A user as listed in Settings.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct UserInfo {
    pub id: i64,
    pub email: String,
    pub invited_by: Option<String>,
    /// When the user signed up; empty while the invitation is pending.
    pub activated_at: Option<DateTime<Utc>>,
    pub passkeys: i64,
}

pub async fn list_users(pool: &PgPool) -> Result<Vec<UserInfo>, sqlx::Error> {
    sqlx::query_as(
        "SELECT u.id, u.email, i.email AS invited_by, u.activated_at,
                (SELECT count(*) FROM passkeys p WHERE p.user_id = u.id) AS passkeys
         FROM users u LEFT JOIN users i ON i.id = u.invited_by
         WHERE u.disabled_at IS NULL
         ORDER BY u.email",
    )
    .fetch_all(pool)
    .await
}

/// Disables a user and closes their sessions. Returns false if there is
/// no such enabled user.
pub async fn disable_user(pool: &PgPool, user_id: i64) -> Result<bool, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let disabled =
        sqlx::query("UPDATE users SET disabled_at = now() WHERE id = $1 AND disabled_at IS NULL")
            .bind(user_id)
            .execute(&mut *tx)
            .await?
            .rows_affected()
            == 1;
    sqlx::query("UPDATE sessions SET revoked_at = now() WHERE user_id = $1 AND revoked_at IS NULL")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(disabled)
}
