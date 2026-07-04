//! Item persistence: insert, dedup, pgvector search, retry queue.

use crate::models::{ContextWindow, RetrievedItem};
use anyhow::Result;
use chrono::{DateTime, Utc};
use pgvector::Vector;
use sqlx::PgPool;
use uuid::Uuid;

pub struct NewItem<'a> {
    pub group_id: i64,
    pub owner_user_id: i64,
    pub shared_by: i64,
    pub source_channel: &'a str,
    pub url: &'a str,
    pub message_id: i64,
    pub title: Option<&'a str>,
    pub raw_content: Option<&'a str>,
    pub summary: Option<&'a str>,
    pub tags: &'a [String],
    pub category: Option<&'a str>,
    pub context_window: &'a ContextWindow,
    pub context_signals: Option<&'a crate::models::ContextSignals>,
    pub embedding: Option<&'a [f32]>,
    pub fetch_status: &'a str,
    pub content_type: &'a str,
}

/// True if this URL was already ingested for the group within `dedup_days`.
pub async fn is_duplicate(
    pool: &PgPool,
    group_id: i64,
    url: &str,
    dedup_days: i64,
) -> Result<bool> {
    let row: Option<(i64,)> = sqlx::query_as(
        r#"
        SELECT 1 FROM items
        WHERE group_id = $1 AND url = $2
          AND shared_at > NOW() - ($3 || ' days')::interval
        LIMIT 1
        "#,
    )
    .bind(group_id)
    .bind(url)
    .bind(dedup_days.to_string())
    .fetch_optional(pool)
    .await?;
    Ok(row.is_some())
}

/// Insert an item. Returns its id, or None if it collided with the daily dedup index.
pub async fn insert(pool: &PgPool, item: NewItem<'_>) -> Result<Option<Uuid>> {
    let embedding = item.embedding.map(|e| Vector::from(e.to_vec()));
    let ctx = serde_json::to_value(item.context_window)?;
    let signals = item
        .context_signals
        .map(serde_json::to_value)
        .transpose()?;

    let row: Option<(Uuid,)> = sqlx::query_as(
        r#"
        INSERT INTO items
            (group_id, owner_user_id, shared_by, source_channel, url, message_id,
             title, raw_content, summary, tags, category, context_window,
             context_signals, embedding, fetch_status, content_type, source)
        VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$4)
        ON CONFLICT DO NOTHING
        RETURNING id
        "#,
    )
    .bind(item.group_id)
    .bind(item.owner_user_id)
    .bind(item.shared_by)
    .bind(item.source_channel)
    .bind(item.url)
    .bind(item.message_id)
    .bind(item.title)
    .bind(item.raw_content)
    .bind(item.summary)
    .bind(item.tags)
    .bind(item.category)
    .bind(ctx)
    .bind(signals)
    .bind(embedding)
    .bind(item.fetch_status)
    .bind(item.content_type)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|(id,)| id))
}

/// Top-k items in a group by cosine similarity to `query`, above `min_similarity`.
pub async fn search(
    pool: &PgPool,
    group_id: i64,
    query: &[f32],
    limit: i64,
    min_similarity: f32,
) -> Result<Vec<RetrievedItem>> {
    let qvec = Vector::from(query.to_vec());

    // 1 - cosine_distance == cosine_similarity
    let rows: Vec<ItemRow> = sqlx::query_as(
        r#"
        SELECT i.id, i.url, i.title, i.summary, i.raw_content, i.tags, i.category,
               i.context_window, i.shared_by, u.username, i.message_id, i.shared_at,
               i.source_channel, i.content_type, i.group_id,
               1 - (i.embedding <=> $2) AS similarity
        FROM items i
        LEFT JOIN users u ON u.id = i.shared_by
        WHERE i.group_id = $1 AND i.embedding IS NOT NULL
          AND 1 - (i.embedding <=> $2) > $3
        ORDER BY i.embedding <=> $2 ASC
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

/// Items awaiting graph processing, oldest-first.
pub async fn unprocessed(pool: &PgPool, limit: i64) -> Result<Vec<GraphItem>> {
    let rows: Vec<(Uuid, i64, Option<String>, Option<String>, Vec<String>)> = sqlx::query_as(
        r#"
        SELECT id, group_id, title, summary, tags
        FROM items
        WHERE graph_processed = FALSE
        ORDER BY created_at ASC
        LIMIT $1
        "#,
    )
    .bind(limit)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(id, group_id, title, summary, tags)| GraphItem {
            id,
            group_id,
            title,
            summary,
            tags,
        })
        .collect())
}

pub async fn mark_graph_processed(pool: &PgPool, ids: &[Uuid]) -> Result<()> {
    sqlx::query("UPDATE items SET graph_processed = TRUE WHERE id = ANY($1)")
        .bind(ids)
        .execute(pool)
        .await?;
    Ok(())
}

/// Items stuck in pending_retry, for the 15-min sweep.
pub async fn pending_retries(pool: &PgPool, limit: i64) -> Result<Vec<(Uuid, i64, String)>> {
    let rows: Vec<(Uuid, i64, String)> = sqlx::query_as(
        r#"
        SELECT id, group_id, url FROM items
        WHERE fetch_status = 'pending_retry'
        ORDER BY created_at ASC
        LIMIT $1
        "#,
    )
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Update an item after a successful retry fetch.
pub async fn update_after_fetch(
    pool: &PgPool,
    id: Uuid,
    title: Option<&str>,
    raw_content: &str,
    summary: &str,
    tags: &[String],
    category: Option<&str>,
    embedding: Option<&[f32]>,
) -> Result<()> {
    let emb = embedding.map(|e| Vector::from(e.to_vec()));
    sqlx::query(
        r#"
        UPDATE items
        SET title = COALESCE($2, title),
            raw_content = $3,
            summary = $4,
            tags = $5,
            category = $6,
            embedding = COALESCE($7, embedding),
            fetch_status = 'ok'
        WHERE id = $1
        "#,
    )
    .bind(id)
    .bind(title)
    .bind(raw_content)
    .bind(summary)
    .bind(tags)
    .bind(category)
    .bind(emb)
    .execute(pool)
    .await?;
    Ok(())
}

/// Give up on a retry after repeated failure: keep it as a permanent stub.
pub async fn mark_retry_exhausted(pool: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query("UPDATE items SET fetch_status = 'unavailable' WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

#[allow(dead_code)] // used by future retry/inspection paths
pub async fn raw_content(pool: &PgPool, id: Uuid) -> Result<Option<String>> {
    let row: Option<(Option<String>,)> =
        sqlx::query_as("SELECT raw_content FROM items WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await?;
    Ok(row.and_then(|(c,)| c))
}

/// Embedding + tags for taste feedback on a specific item.
pub async fn signal_source(pool: &PgPool, id: Uuid) -> Result<Option<(Vec<f32>, Vec<String>)>> {
    let row: Option<(Option<Vector>, Vec<String>)> = sqlx::query_as(
        "SELECT embedding, tags FROM items WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(emb, tags)| (emb.map(|v| v.to_vec()).unwrap_or_default(), tags)))
}

/// Load items by id (for session follow-up referents).
pub async fn by_ids(pool: &PgPool, ids: &[Uuid]) -> Result<Vec<RetrievedItem>> {
    if ids.is_empty() {
        return Ok(vec![]);
    }
    let rows: Vec<ItemRow> = sqlx::query_as(
        r#"
        SELECT i.id, i.url, i.title, i.summary, i.raw_content, i.tags, i.category,
               i.context_window, i.shared_by, u.username, i.message_id, i.shared_at,
               i.source_channel, i.content_type, i.group_id,
               1.0 AS similarity
        FROM items i
        LEFT JOIN users u ON u.id = i.shared_by
        WHERE i.id = ANY($1)
        ORDER BY i.shared_at DESC
        "#,
    )
    .bind(ids)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(Into::into).collect())
}

/// Item captured from a specific Telegram message (reply-to anchor).
pub async fn by_message(
    pool: &PgPool,
    group_id: i64,
    message_id: i64,
) -> Result<Option<RetrievedItem>> {
    let row: Option<ItemRow> = sqlx::query_as(
        r#"
        SELECT i.id, i.url, i.title, i.summary, i.raw_content, i.tags, i.category,
               i.context_window, i.shared_by, u.username, i.message_id, i.shared_at,
               i.source_channel, i.content_type, i.group_id,
               1.0 AS similarity
        FROM items i
        LEFT JOIN users u ON u.id = i.shared_by
        WHERE i.group_id = $1 AND i.message_id = $2
        ORDER BY i.shared_at DESC
        LIMIT 1
        "#,
    )
    .bind(group_id)
    .bind(message_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(Into::into))
}

/// Most recent capture by this user in a chat (for "this"/"that" without reply).
pub async fn latest_in_chat(
    pool: &PgPool,
    group_id: i64,
    owner_user_id: i64,
    within_minutes: i64,
) -> Result<Option<RetrievedItem>> {
    let row: Option<ItemRow> = sqlx::query_as(
        r#"
        SELECT i.id, i.url, i.title, i.summary, i.raw_content, i.tags, i.category,
               i.context_window, i.shared_by, u.username, i.message_id, i.shared_at,
               i.source_channel, i.content_type, i.group_id,
               1.0 AS similarity
        FROM items i
        LEFT JOIN users u ON u.id = i.shared_by
        WHERE i.group_id = $1 AND i.owner_user_id = $2
          AND i.shared_at > NOW() - ($3 || ' minutes')::interval
        ORDER BY i.shared_at DESC
        LIMIT 1
        "#,
    )
    .bind(group_id)
    .bind(owner_user_id)
    .bind(within_minutes.to_string())
    .fetch_optional(pool)
    .await?;
    Ok(row.map(Into::into))
}

/// Fuzzy URL match within a chat (handles truncated preview links).
pub async fn by_url_in_chat(
    pool: &PgPool,
    group_id: i64,
    url: &str,
) -> Result<Option<RetrievedItem>> {
    let row: Option<ItemRow> = sqlx::query_as(
        r#"
        SELECT i.id, i.url, i.title, i.summary, i.raw_content, i.tags, i.category,
               i.context_window, i.shared_by, u.username, i.message_id, i.shared_at,
               i.source_channel, i.content_type, i.group_id,
               1.0 AS similarity
        FROM items i
        LEFT JOIN users u ON u.id = i.shared_by
        WHERE i.group_id = $1
          AND (i.url = $2 OR i.url LIKE $2 || '%' OR $2 LIKE i.url || '%')
        ORDER BY i.shared_at DESC
        LIMIT 1
        "#,
    )
    .bind(group_id)
    .bind(url)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(Into::into))
}

/// Most recent capture in a chat (for "this"/"that" without reply).
pub async fn latest_in_chat_any(
    pool: &PgPool,
    group_id: i64,
    within_minutes: i64,
) -> Result<Option<RetrievedItem>> {
    let row: Option<ItemRow> = sqlx::query_as(
        r#"
        SELECT i.id, i.url, i.title, i.summary, i.raw_content, i.tags, i.category,
               i.context_window, i.shared_by, u.username, i.message_id, i.shared_at,
               i.source_channel, i.content_type, i.group_id,
               1.0 AS similarity
        FROM items i
        LEFT JOIN users u ON u.id = i.shared_by
        WHERE i.group_id = $1
          AND i.shared_at > NOW() - ($2 || ' minutes')::interval
        ORDER BY i.shared_at DESC
        LIMIT 1
        "#,
    )
    .bind(group_id)
    .bind(within_minutes.to_string())
    .fetch_optional(pool)
    .await?;
    Ok(row.map(Into::into))
}

// ── internal row mapping ────────────────────────────────────────────────────

#[derive(sqlx::FromRow)]
struct ItemRow {
    id: Uuid,
    url: String,
    title: Option<String>,
    summary: Option<String>,
    raw_content: Option<String>,
    tags: Vec<String>,
    category: Option<String>,
    context_window: Option<serde_json::Value>,
    shared_by: Option<i64>,
    username: Option<String>,
    message_id: Option<i64>,
    shared_at: DateTime<Utc>,
    source_channel: Option<String>,
    content_type: Option<String>,
    group_id: Option<i64>,
    similarity: Option<f64>,
}

impl From<ItemRow> for RetrievedItem {
    fn from(r: ItemRow) -> Self {
        RetrievedItem {
            id: r.id,
            url: r.url,
            title: r.title,
            summary: r.summary,
            raw_content: r.raw_content,
            tags: r.tags,
            category: r.category,
            context_window: r
                .context_window
                .and_then(|v| serde_json::from_value::<ContextWindow>(v).ok()),
            shared_by: r.shared_by,
            shared_by_username: r.username,
            message_id: r.message_id,
            shared_at: r.shared_at,
            similarity: r.similarity.unwrap_or(0.0) as f32,
            source_channel: r.source_channel,
            content_type: r.content_type,
            group_id: r.group_id,
        }
    }
}

pub struct GraphItem {
    pub id: Uuid,
    pub group_id: i64,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub tags: Vec<String>,
}
