//! Edge insertion for the knowledge graph.

use anyhow::Result;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

#[allow(clippy::too_many_arguments)]
pub async fn insert(
    pool: &PgPool,
    group_id: i64,
    source_id: Uuid,
    source_type: &str,
    target_id: Uuid,
    target_type: &str,
    relationship: &str,
    strength: f32,
    metadata: Option<Value>,
) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO edges
            (group_id, source_id, source_type, target_id, target_type,
             relationship, strength, metadata)
        VALUES ($1,$2,$3,$4,$5,$6,$7,$8)
        "#,
    )
    .bind(group_id)
    .bind(source_id)
    .bind(source_type)
    .bind(target_id)
    .bind(target_type)
    .bind(relationship)
    .bind(strength as f64)
    .bind(metadata)
    .execute(pool)
    .await?;
    Ok(())
}
