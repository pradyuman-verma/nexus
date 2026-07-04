//! Group + user upsert and the rolling messages buffer.

use crate::models::ContextMessage;
use anyhow::Result;
use sqlx::PgPool;

/// Upsert a Telegram group, refreshing its name if we now know it.
/// (Non-Telegram spaces go through `db::channels::resolve_space`.)
pub async fn upsert_group(pool: &PgPool, id: i64, name: Option<&str>) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO groups (id, name, channel, external_id)
        VALUES ($1, $2, 'telegram', $1::text)
        ON CONFLICT (id) DO UPDATE
        SET name = COALESCE(EXCLUDED.name, groups.name)
        "#,
    )
    .bind(id)
    .bind(name)
    .execute(pool)
    .await?;
    Ok(())
}

/// Upsert a Telegram user, refreshing username / first_name.
/// (Non-Telegram users go through `db::channels::resolve_user`.)
pub async fn upsert_user(
    pool: &PgPool,
    id: i64,
    username: Option<&str>,
    first_name: Option<&str>,
) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO users (id, username, first_name, channel, external_id)
        VALUES ($1, $2, $3, 'telegram', $1::text)
        ON CONFLICT (id) DO UPDATE
        SET username   = COALESCE(EXCLUDED.username, users.username),
            first_name = COALESCE(EXCLUDED.first_name, users.first_name)
        "#,
    )
    .bind(id)
    .bind(username)
    .bind(first_name)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn first_name(pool: &PgPool, id: i64) -> Result<Option<String>> {
    let row: Option<(Option<String>, Option<String>)> =
        sqlx::query_as("SELECT first_name, username FROM users WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(fname, uname)| fname.or(uname).unwrap_or_else(|| "Someone".to_string())))
}

/// Count buffered messages in a chat (for privacy nudge on first link).
pub async fn buffer_count(pool: &PgPool, group_id: i64) -> Result<i64> {
    let (n,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM messages_buffer WHERE group_id = $1",
    )
    .bind(group_id)
    .fetch_one(pool)
    .await?;
    Ok(n)
}

/// Append a message to the rolling buffer.
pub async fn buffer_message(
    pool: &PgPool,
    group_id: i64,
    user_id: Option<i64>,
    username: Option<&str>,
    message_id: i64,
    text: Option<&str>,
) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO messages_buffer (group_id, user_id, username, message_id, text)
        VALUES ($1, $2, $3, $4, $5)
        "#,
    )
    .bind(group_id)
    .bind(user_id)
    .bind(username)
    .bind(message_id)
    .bind(text)
    .execute(pool)
    .await?;
    Ok(())
}

/// The `n` most recent buffered messages strictly before `message_id`, oldest-first.
pub async fn messages_before(
    pool: &PgPool,
    group_id: i64,
    message_id: i64,
    n: i64,
) -> Result<Vec<ContextMessage>> {
    let rows: Vec<(Option<i64>, Option<String>, i64, Option<String>)> = sqlx::query_as(
        r#"
        SELECT user_id, username, message_id, text
        FROM messages_buffer
        WHERE group_id = $1 AND message_id < $2
        ORDER BY message_id DESC
        LIMIT $3
        "#,
    )
    .bind(group_id)
    .bind(message_id)
    .bind(n)
    .fetch_all(pool)
    .await?;

    let mut out: Vec<ContextMessage> = rows
        .into_iter()
        .map(|(user_id, username, mid, text)| ContextMessage {
            user_id,
            username,
            message_id: mid,
            text: text.unwrap_or_default(),
            position: crate::models::ContextPosition::Before,
        })
        .collect();
    out.reverse(); // oldest-first
    Ok(out)
}

/// Up to `n` buffered messages strictly after `message_id`, oldest-first.
pub async fn messages_after(
    pool: &PgPool,
    group_id: i64,
    message_id: i64,
    n: i64,
) -> Result<Vec<ContextMessage>> {
    let rows: Vec<(Option<i64>, Option<String>, i64, Option<String>)> = sqlx::query_as(
        r#"
        SELECT user_id, username, message_id, text
        FROM messages_buffer
        WHERE group_id = $1 AND message_id > $2
        ORDER BY message_id ASC
        LIMIT $3
        "#,
    )
    .bind(group_id)
    .bind(message_id)
    .bind(n)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(user_id, username, mid, text)| ContextMessage {
            user_id,
            username,
            message_id: mid,
            text: text.unwrap_or_default(),
            position: crate::models::ContextPosition::After,
        })
        .collect())
}

/// Delete buffer rows older than 48h. Returns rows removed.
pub async fn purge_old_buffer(pool: &PgPool) -> Result<u64> {
    let res = sqlx::query(
        "DELETE FROM messages_buffer WHERE created_at < NOW() - INTERVAL '48 hours'",
    )
    .execute(pool)
    .await?;
    Ok(res.rows_affected())
}
