//! Graph lookups used to expand `/ask` retrieval beyond pure vector similarity.

use anyhow::Result;
use sqlx::PgPool;
use uuid::Uuid;

/// Entities whose name appears in the query text (case-insensitive). The seed
/// set for graph expansion. Names shorter than 3 chars are ignored to avoid noise.
pub async fn entities_in_query(
    pool: &PgPool,
    group_id: i64,
    query: &str,
    limit: i64,
) -> Result<Vec<Uuid>> {
    let rows: Vec<(Uuid,)> = sqlx::query_as(
        r#"
        SELECT id FROM entities
        WHERE group_id = $1
          AND char_length(name) >= 3
          AND lower($2) LIKE '%' || lower(name) || '%'
        ORDER BY last_seen DESC
        LIMIT $3
        "#,
    )
    .bind(group_id)
    .bind(query)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

/// One hop of entity↔entity neighbours of `seeds` (the connected-topic signal).
pub async fn related_entities(
    pool: &PgPool,
    group_id: i64,
    seeds: &[Uuid],
    limit: i64,
) -> Result<Vec<Uuid>> {
    if seeds.is_empty() {
        return Ok(Vec::new());
    }
    let rows: Vec<(Uuid,)> = sqlx::query_as(
        r#"
        SELECT DISTINCT
          CASE WHEN source_id = ANY($2) THEN target_id ELSE source_id END AS neighbour
        FROM edges
        WHERE group_id = $1
          AND source_type = 'entity' AND target_type = 'entity'
          AND (source_id = ANY($2) OR target_id = ANY($2))
        LIMIT $3
        "#,
    )
    .bind(group_id)
    .bind(seeds)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

/// Item ids that mention any of `entity_ids` (via item→entity 'mentions' edges).
pub async fn items_mentioning(
    pool: &PgPool,
    group_id: i64,
    entity_ids: &[Uuid],
    limit: i64,
) -> Result<Vec<Uuid>> {
    if entity_ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows: Vec<(Uuid,)> = sqlx::query_as(
        r#"
        SELECT DISTINCT source_id FROM edges
        WHERE group_id = $1
          AND source_type = 'item' AND target_type = 'entity'
          AND relationship = 'mentions'
          AND target_id = ANY($2)
        LIMIT $3
        "#,
    )
    .bind(group_id)
    .bind(entity_ids)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}
