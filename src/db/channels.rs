//! Channel-identity resolution: maps a channel's native id space
//! (phone numbers, IG handles, …) onto Nexus's internal BIGINT ids.
//! Telegram bypasses this — its native ids ARE the internal ids.

use anyhow::Result;
use sqlx::PgPool;

/// Record a webhook event id; returns false when it was already seen
/// (redelivery) and the caller should drop the event.
pub async fn record_event(pool: &PgPool, channel: &str, event_id: &str) -> Result<bool> {
    let res = sqlx::query(
        r#"
        INSERT INTO channel_events (channel, event_id) VALUES ($1, $2)
        ON CONFLICT DO NOTHING
        "#,
    )
    .bind(channel)
    .bind(event_id)
    .execute(pool)
    .await?;
    Ok(res.rows_affected() > 0)
}

/// Get-or-create the space (groups row) for a channel-native chat id.
/// New spaces get a synthetic internal id from the sequence.
pub async fn resolve_space(
    pool: &PgPool,
    channel: &str,
    external_id: &str,
    name: Option<&str>,
) -> Result<i64> {
    let (id,): (i64,) = sqlx::query_as(
        r#"
        INSERT INTO groups (id, name, channel, external_id)
        VALUES (nextval('synthetic_id_seq'), $3, $1, $2)
        ON CONFLICT (channel, external_id) DO UPDATE
        SET name = COALESCE(EXCLUDED.name, groups.name)
        RETURNING id
        "#,
    )
    .bind(channel)
    .bind(external_id)
    .bind(name)
    .fetch_one(pool)
    .await?;
    Ok(id)
}

/// Get-or-create the user for a channel-native user id.
pub async fn resolve_user(
    pool: &PgPool,
    channel: &str,
    external_id: &str,
    display_name: Option<&str>,
) -> Result<i64> {
    let (id,): (i64,) = sqlx::query_as(
        r#"
        INSERT INTO users (id, first_name, channel, external_id)
        VALUES (nextval('synthetic_id_seq'), $3, $1, $2)
        ON CONFLICT (channel, external_id) DO UPDATE
        SET first_name = COALESCE(EXCLUDED.first_name, users.first_name)
        RETURNING id
        "#,
    )
    .bind(channel)
    .bind(external_id)
    .bind(display_name)
    .fetch_one(pool)
    .await?;
    Ok(id)
}

/// The channel-native address for an internal group id — how outbound
/// replies find their way back to the right transport.
pub async fn space_address(pool: &PgPool, group_id: i64) -> Result<Option<(String, String)>> {
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT channel, external_id FROM groups WHERE id = $1")
            .bind(group_id)
            .fetch_optional(pool)
            .await?;
    Ok(row)
}
