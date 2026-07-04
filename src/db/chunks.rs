//! Passage-chunk persistence + vector search for chunk-level RAG.

use anyhow::Result;
use chrono::{DateTime, Utc};
use pgvector::Vector;
use sqlx::PgPool;
use uuid::Uuid;

/// One retrieved passage with its parent item's metadata.
#[derive(Debug, Clone)]
pub struct ChunkHit {
    pub id: Uuid,
    pub item_id: Uuid,
    pub url: String,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub shared_by: Option<i64>,
    pub username: Option<String>,
    pub message_id: Option<i64>,
    pub shared_at: DateTime<Utc>,
    pub content: String,
    pub similarity: f32,
    pub source_channel: Option<String>,
    pub content_type: Option<String>,
}

/// Replace an item's chunks with a fresh set. `rows` is `(index, content, embedding)`.
pub async fn replace_for_item(
    pool: &PgPool,
    item_id: Uuid,
    group_id: i64,
    owner_user_id: i64,
    rows: &[(i32, String, Vec<f32>)],
) -> Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM chunks WHERE item_id = $1")
        .bind(item_id)
        .execute(&mut *tx)
        .await?;

    for (idx, content, embedding) in rows {
        let vec = Vector::from(embedding.clone());
        sqlx::query(
            r#"
            INSERT INTO chunks (item_id, group_id, owner_user_id, chunk_index, content, embedding)
            VALUES ($1, $2, $3, $4, $5, $6)
            "#,
        )
        .bind(item_id)
        .bind(group_id)
        .bind(owner_user_id)
        .bind(idx)
        .bind(content)
        .bind(vec)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Top-k passages in a group by cosine similarity to `query`, above `min_similarity`.
pub async fn search(
    pool: &PgPool,
    group_id: i64,
    query: &[f32],
    limit: i64,
    min_similarity: f32,
) -> Result<Vec<ChunkHit>> {
    let qvec = Vector::from(query.to_vec());
    let rows: Vec<ChunkRow> = sqlx::query_as(
        r#"
        SELECT c.id, c.item_id, i.url, i.title, i.summary, i.shared_by, u.username,
               i.message_id, i.shared_at, c.content,
               i.source_channel, i.content_type,
               1 - (c.embedding <=> $2) AS similarity
        FROM chunks c
        JOIN items i ON i.id = c.item_id
        LEFT JOIN users u ON u.id = i.shared_by
        WHERE c.group_id = $1 AND c.embedding IS NOT NULL
          AND 1 - (c.embedding <=> $2) > $3
        ORDER BY c.embedding <=> $2 ASC
        LIMIT $4
        "#,
    )
    .bind(group_id)
    .bind(qvec)
    .bind(min_similarity as f64)
    .bind(limit)
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(Into::into).collect())
}

/// Top-k passages in a user's personal corpus by cosine similarity.
pub async fn search_by_owner(
    pool: &PgPool,
    owner_user_id: i64,
    query: &[f32],
    limit: i64,
    min_similarity: f32,
) -> Result<Vec<ChunkHit>> {
    let qvec = Vector::from(query.to_vec());
    let rows: Vec<ChunkRow> = sqlx::query_as(
        r#"
        SELECT c.id, c.item_id, i.url, i.title, i.summary, i.shared_by, u.username,
               i.message_id, i.shared_at, c.content,
               i.source_channel, i.content_type,
               1 - (c.embedding <=> $2) AS similarity
        FROM chunks c
        JOIN items i ON i.id = c.item_id
        LEFT JOIN users u ON u.id = i.shared_by
        WHERE c.owner_user_id = $1 AND c.embedding IS NOT NULL
          AND 1 - (c.embedding <=> $2) > $3
        ORDER BY c.embedding <=> $2 ASC
        LIMIT $4
        "#,
    )
    .bind(owner_user_id)
    .bind(qvec)
    .bind(min_similarity as f64)
    .bind(limit)
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(Into::into).collect())
}

/// Keyword search over a user's personal corpus.
pub async fn keyword_search_by_owner(
    pool: &PgPool,
    owner_user_id: i64,
    query: &str,
    limit: i64,
) -> Result<Vec<ChunkHit>> {
    let rows: Vec<ChunkRow> = sqlx::query_as(
        r#"
        WITH q AS (
            SELECT replace(plainto_tsquery('english', $2)::text, '&', '|')::tsquery AS tsq
        )
        SELECT c.id, c.item_id, i.url, i.title, i.summary, i.shared_by, u.username,
               i.message_id, i.shared_at, c.content,
               i.source_channel, i.content_type,
               ts_rank(to_tsvector('english', c.content), (SELECT tsq FROM q)) AS similarity
        FROM chunks c
        JOIN items i ON i.id = c.item_id
        LEFT JOIN users u ON u.id = i.shared_by
        WHERE c.owner_user_id = $1
          AND (SELECT tsq FROM q) != ''::tsquery
          AND to_tsvector('english', c.content) @@ (SELECT tsq FROM q)
        ORDER BY similarity DESC
        LIMIT $3
        "#,
    )
    .bind(owner_user_id)
    .bind(query)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(Into::into).collect())
}

/// Best-matching passages restricted to a specific set of items, with NO
/// similarity floor — used by graph expansion to surface graph-connected items
/// the vector search would otherwise have filtered out.
pub async fn search_within_items(
    pool: &PgPool,
    group_id: i64,
    query: &[f32],
    item_ids: &[Uuid],
    limit: i64,
) -> Result<Vec<ChunkHit>> {
    if item_ids.is_empty() {
        return Ok(Vec::new());
    }
    let qvec = Vector::from(query.to_vec());
    let rows: Vec<ChunkRow> = sqlx::query_as(
        r#"
        SELECT c.id, c.item_id, i.url, i.title, i.summary, i.shared_by, u.username,
               i.message_id, i.shared_at, c.content,
               i.source_channel, i.content_type,
               1 - (c.embedding <=> $2) AS similarity
        FROM chunks c
        JOIN items i ON i.id = c.item_id
        LEFT JOIN users u ON u.id = i.shared_by
        WHERE c.group_id = $1 AND c.embedding IS NOT NULL AND c.item_id = ANY($3)
        ORDER BY c.embedding <=> $2 ASC
        LIMIT $4
        "#,
    )
    .bind(group_id)
    .bind(qvec)
    .bind(item_ids)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(Into::into).collect())
}

/// Keyword (full-text) search over passages — the lexical half of hybrid search.
///
/// Uses OR semantics: `plainto_tsquery` ANDs terms, which is wrong for natural
/// questions (no passage contains every word), so we rewrite `&` → `|` to match
/// passages containing ANY significant term, ranked by `ts_rank`. Precision is
/// recovered by RRF fusion + the model's answerability gate. `similarity` carries
/// `ts_rank` (used only for fusion ordering).
pub async fn keyword_search(
    pool: &PgPool,
    group_id: i64,
    query: &str,
    limit: i64,
) -> Result<Vec<ChunkHit>> {
    let rows: Vec<ChunkRow> = sqlx::query_as(
        r#"
        WITH q AS (
            SELECT replace(plainto_tsquery('english', $2)::text, '&', '|')::tsquery AS tsq
        )
        SELECT c.id, c.item_id, i.url, i.title, i.summary, i.shared_by, u.username,
               i.message_id, i.shared_at, c.content,
               i.source_channel, i.content_type,
               ts_rank(to_tsvector('english', c.content), (SELECT tsq FROM q)) AS similarity
        FROM chunks c
        JOIN items i ON i.id = c.item_id
        LEFT JOIN users u ON u.id = i.shared_by
        WHERE c.group_id = $1
          AND (SELECT tsq FROM q) != ''::tsquery
          AND to_tsvector('english', c.content) @@ (SELECT tsq FROM q)
        ORDER BY similarity DESC
        LIMIT $3
        "#,
    )
    .bind(group_id)
    .bind(query)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(Into::into).collect())
}

/// Items that have no chunks yet (for backfilling pre-chunking ingests).
pub async fn items_missing_chunks(
    pool: &PgPool,
    group_id: i64,
    limit: i64,
) -> Result<Vec<MissingItem>> {
    let rows: Vec<(Uuid, Option<String>, Option<String>, Option<String>, Option<i64>)> =
        sqlx::query_as(
        r#"
        SELECT i.id, i.title, i.summary, i.raw_content, i.owner_user_id
        FROM items i
        WHERE i.group_id = $1
          AND NOT EXISTS (SELECT 1 FROM chunks c WHERE c.item_id = i.id)
        ORDER BY i.created_at DESC
        LIMIT $2
        "#,
    )
    .bind(group_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(id, title, summary, raw_content, owner_user_id)| MissingItem {
            id,
            title,
            summary,
            raw_content,
            owner_user_id,
        })
        .collect())
}

pub struct MissingItem {
    pub id: Uuid,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub raw_content: Option<String>,
    pub owner_user_id: Option<i64>,
}

#[derive(sqlx::FromRow)]
struct ChunkRow {
    id: Uuid,
    item_id: Uuid,
    url: String,
    title: Option<String>,
    summary: Option<String>,
    shared_by: Option<i64>,
    username: Option<String>,
    message_id: Option<i64>,
    shared_at: DateTime<Utc>,
    content: String,
    source_channel: Option<String>,
    content_type: Option<String>,
    similarity: Option<f64>,
}

impl From<ChunkRow> for ChunkHit {
    fn from(r: ChunkRow) -> Self {
        ChunkHit {
            id: r.id,
            item_id: r.item_id,
            url: r.url,
            title: r.title,
            summary: r.summary,
            shared_by: r.shared_by,
            username: r.username,
            message_id: r.message_id,
            shared_at: r.shared_at,
            content: r.content,
            similarity: r.similarity.unwrap_or(0.0) as f32,
            source_channel: r.source_channel,
            content_type: r.content_type,
        }
    }
}
