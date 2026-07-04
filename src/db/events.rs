//! Layer 2 event stream — every user action that teaches the taste model.

use anyhow::Result;
use sqlx::PgPool;
use uuid::Uuid;

/// Known event types for the behavioral feature store.
pub mod event_type {
    pub const CAPTURE: &str = "capture";
    pub const QUERY: &str = "query";
    pub const NOTIFY_SENT: &str = "notify_sent";
    pub const NOTIFY_DISMISS: &str = "notify_dismiss";
    pub const ASK_THUMBS_UP: &str = "ask_thumbs_up";
    pub const ASK_THUMBS_DOWN: &str = "ask_thumbs_down";
}

/// Append one behavioral event. Failures are logged by callers, never propagated
/// to user-facing paths.
pub async fn log(
    pool: &PgPool,
    user_id: i64,
    event_type: &str,
    item_id: Option<Uuid>,
    signal: f32,
    metadata: serde_json::Value,
) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO user_events (user_id, event_type, item_id, signal, metadata)
        VALUES ($1, $2, $3, $4, $5)
        "#,
    )
    .bind(user_id)
    .bind(event_type)
    .bind(item_id)
    .bind(signal as f64)
    .bind(metadata)
    .execute(pool)
    .await?;
    Ok(())
}

/// Count events of a type for a user (used by stats / calibration).
pub async fn count_for_user(pool: &PgPool, user_id: i64, event_type: &str) -> Result<i64> {
    let (n,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM user_events WHERE user_id = $1 AND event_type = $2",
    )
    .bind(user_id)
    .bind(event_type)
    .fetch_one(pool)
    .await?;
    Ok(n)
}

/// Count events of a type in the last `days` days (rolling window).
pub async fn count_since(
    pool: &PgPool,
    user_id: i64,
    event_type: &str,
    days: i64,
) -> Result<i64> {
    let (n,): (i64,) = sqlx::query_as(
        r#"
        SELECT COUNT(*) FROM user_events
        WHERE user_id = $1 AND event_type = $2
          AND occurred_at > NOW() - ($3 || ' days')::interval
        "#,
    )
    .bind(user_id)
    .bind(event_type)
    .bind(days.to_string())
    .fetch_one(pool)
    .await?;
    Ok(n)
}

/// Users with at least one event of `event_type` in the last `days` days.
pub async fn users_with_event_since(
    pool: &PgPool,
    event_type: &str,
    days: i64,
) -> Result<Vec<i64>> {
    let rows: Vec<(i64,)> = sqlx::query_as(
        r#"
        SELECT DISTINCT user_id FROM user_events
        WHERE event_type = $1
          AND occurred_at > NOW() - ($2 || ' days')::interval
        "#,
    )
    .bind(event_type)
    .bind(days.to_string())
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}
