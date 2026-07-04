//! Per-chat working memory: recent turns + thread state (active referents).

use crate::models::{SessionTurn, ThreadState};
use anyhow::Result;
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

const MAX_TURNS_STORED: i64 = 24;
const MAX_TURNS_FOR_PROMPT: usize = 6;

pub struct ChatSession {
    pub id: Uuid,
    pub thread_state: ThreadState,
}

/// Load or create the session for `(user_id, chat_id)`.
pub async fn get_or_create(pool: &PgPool, user_id: i64, chat_id: i64) -> Result<ChatSession> {
    let row: Option<(Uuid, serde_json::Value)> = sqlx::query_as(
        r#"
        SELECT id, thread_state
        FROM chat_sessions
        WHERE user_id = $1 AND chat_id = $2
        "#,
    )
    .bind(user_id)
    .bind(chat_id)
    .fetch_optional(pool)
    .await?;

    if let Some((id, state)) = row {
        let thread_state: ThreadState = serde_json::from_value(state).unwrap_or_default();
        return Ok(ChatSession { id, thread_state });
    }

    let id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO chat_sessions (user_id, chat_id)
        VALUES ($1, $2)
        ON CONFLICT (user_id, chat_id) DO UPDATE SET updated_at = NOW()
        RETURNING id
        "#,
    )
    .bind(user_id)
    .bind(chat_id)
    .fetch_one(pool)
    .await?;

    Ok(ChatSession {
        id,
        thread_state: ThreadState::default(),
    })
}

/// Recent turns oldest-first for the synthesis prompt.
pub async fn recent_turns(pool: &PgPool, session_id: Uuid) -> Result<Vec<SessionTurn>> {
    let rows: Vec<(String, String, Vec<Uuid>, Vec<Uuid>, DateTime<Utc>)> = sqlx::query_as(
        r#"
        SELECT role, text, item_ids, cited_item_ids, created_at
        FROM session_turns
        WHERE session_id = $1
        ORDER BY created_at DESC
        LIMIT $2
        "#,
    )
    .bind(session_id)
    .bind(MAX_TURNS_FOR_PROMPT as i64)
    .fetch_all(pool)
    .await?;

    let mut turns: Vec<SessionTurn> = rows
        .into_iter()
        .map(|(role, text, item_ids, cited_item_ids, created_at)| SessionTurn {
            role,
            text,
            item_ids,
            cited_item_ids,
            created_at,
        })
        .collect();
    turns.reverse();
    Ok(turns)
}

/// Append a user + assistant exchange and trim old turns.
pub async fn append_exchange(
    pool: &PgPool,
    session_id: Uuid,
    user_text: &str,
    user_item_ids: &[Uuid],
    assistant_text: &str,
    cited_item_ids: &[Uuid],
    thread_state: &ThreadState,
) -> Result<()> {
    let mut tx = pool.begin().await?;

    sqlx::query(
        r#"
        INSERT INTO session_turns (session_id, role, text, item_ids)
        VALUES ($1, 'user', $2, $3)
        "#,
    )
    .bind(session_id)
    .bind(user_text)
    .bind(user_item_ids)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        r#"
        INSERT INTO session_turns (session_id, role, text, cited_item_ids)
        VALUES ($1, 'assistant', $2, $3)
        "#,
    )
    .bind(session_id)
    .bind(assistant_text)
    .bind(cited_item_ids)
    .execute(&mut *tx)
    .await?;

    let state_json = serde_json::to_value(thread_state)?;
    sqlx::query(
        r#"
        UPDATE chat_sessions
        SET thread_state = $2, updated_at = NOW()
        WHERE id = $1
        "#,
    )
    .bind(session_id)
    .bind(state_json)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        r#"
        DELETE FROM session_turns
        WHERE session_id = $1
          AND id NOT IN (
              SELECT id FROM session_turns
              WHERE session_id = $1
              ORDER BY created_at DESC
              LIMIT $2
          )
        "#,
    )
    .bind(session_id)
    .bind(MAX_TURNS_STORED)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(())
}
