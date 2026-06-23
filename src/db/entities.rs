//! Entity upsert for the knowledge graph.

use anyhow::Result;
use sqlx::PgPool;
use uuid::Uuid;

/// Upsert an entity by (group, name, type); refreshes last_seen. Returns its id.
pub async fn upsert(
    pool: &PgPool,
    group_id: i64,
    name: &str,
    type_: &str,
) -> Result<Uuid> {
    let (id,): (Uuid,) = sqlx::query_as(
        r#"
        INSERT INTO entities (group_id, name, type)
        VALUES ($1, $2, $3)
        ON CONFLICT (group_id, name, type) DO UPDATE
        SET last_seen = NOW()
        RETURNING id
        "#,
    )
    .bind(group_id)
    .bind(name)
    .bind(type_)
    .fetch_one(pool)
    .await?;
    Ok(id)
}

/// The most recently seen entities in a group, as `(name, type)` — fed back into
/// the Tier 4 prompt so the model can reuse existing entities.
pub async fn recent(pool: &PgPool, group_id: i64, limit: i64) -> Result<Vec<(String, String)>> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        r#"
        SELECT name, type FROM entities
        WHERE group_id = $1
        ORDER BY last_seen DESC
        LIMIT $2
        "#,
    )
    .bind(group_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}
