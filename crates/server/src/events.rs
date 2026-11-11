//! The event log: an insert-only record of what happened and who did it,
//! written by every domain.

use sqlx::PgPool;

/// Appends a row to the event log. Failures are logged, not returned: an
/// event must never make the action it describes fail halfway.
pub async fn log_event(pool: &PgPool, user_id: Option<i64>, kind: &str, detail: &str) {
    let result = sqlx::query("INSERT INTO events (user_id, kind, detail) VALUES ($1, $2, $3)")
        .bind(user_id)
        .bind(kind)
        .bind(detail)
        .execute(pool)
        .await;
    if let Err(error) = result {
        eprintln!("cannot log event {kind}: {error}");
    }
}
