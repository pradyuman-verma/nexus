//! Notification logging + dedup.

use anyhow::Result;
use sqlx::PgPool;
use uuid::Uuid;

/// Already notified this user about this item?
pub async fn already_notified(pool: &PgPool, user_id: i64, item_id: Uuid) -> Result<bool> {
    let row: Option<(i64,)> = sqlx::query_as(
        "SELECT 1 FROM notifications_log WHERE user_id = $1 AND item_id = $2 LIMIT 1",
    )
    .bind(user_id)
    .bind(item_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.is_some())
}

/// Record a notification (or a below-threshold score when `sent = false`).
/// Idempotent on (user_id, item_id).
pub async fn log(
    pool: &PgPool,
    user_id: i64,
    item_id: Uuid,
    score: f32,
    sent: bool,
) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO notifications_log (user_id, item_id, score, sent)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT (user_id, item_id) DO NOTHING
        "#,
    )
    .bind(user_id)
    .bind(item_id)
    .bind(score as f64)
    .bind(sent)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn count_for_group(pool: &PgPool, group_id: i64) -> Result<i64> {
    let (n,): (i64,) = sqlx::query_as(
        r#"
        SELECT COUNT(*)
        FROM notifications_log n
        JOIN items i ON i.id = n.item_id
        WHERE i.group_id = $1 AND n.sent = TRUE
        "#,
    )
    .bind(group_id)
    .fetch_one(pool)
    .await?;
    Ok(n)
}
