//! Cron job bodies: graph building, cleanup, and health/retry sweep.

use crate::db;
use crate::graph::builder;
use crate::ingestion::{fetcher, youtube};
use crate::models::IngestionJob;
use crate::state::AppState;
use anyhow::Result;
use tokio::sync::mpsc;

/// Every 6h — build graph edges from unprocessed items.
pub async fn graph_builder(state: &AppState) {
    match builder::run(state, state.config.ingestion_batch_size).await {
        Ok(s) => tracing::info!(
            items = s.items,
            entities = s.entities,
            edges = s.edges,
            "graph build complete"
        ),
        Err(e) => tracing::error!(error = %e, "graph build failed"),
    }
}

/// Every 24h — purge the messages buffer and log per-group stats.
pub async fn cleanup(state: &AppState) {
    match db::groups::purge_old_buffer(&state.pool).await {
        Ok(n) => tracing::info!(removed = n, "buffer purge complete"),
        Err(e) => tracing::error!(error = %e, "buffer purge failed"),
    }

    if let Err(e) = log_group_stats(state).await {
        tracing::warn!(error = %e, "group stats logging failed");
    }

    match db::taste::calibrate_all(&state.pool, state.config.default_relevance_threshold).await {
        Ok(n) if n > 0 => tracing::info!(raised = n, "taste threshold calibration"),
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "taste calibration failed"),
    }
}

async fn log_group_stats(state: &AppState) -> Result<()> {
    let groups: Vec<(i64, Option<String>)> =
        sqlx::query_as("SELECT id, name FROM groups").fetch_all(&state.pool).await?;

    for (group_id, name) in groups {
        let (total, week): (i64, i64) = sqlx::query_as(
            r#"
            SELECT
              COUNT(*),
              COUNT(*) FILTER (WHERE shared_at > NOW() - INTERVAL '7 days')
            FROM items WHERE group_id = $1
            "#,
        )
        .bind(group_id)
        .fetch_one(&state.pool)
        .await?;

        let notifications = db::notifications::count_for_group(&state.pool, group_id)
            .await
            .unwrap_or(0);

        tracing::info!(
            group_id,
            name = name.as_deref().unwrap_or("?"),
            total_items = total,
            items_this_week = week,
            notifications,
            "group stats"
        );
    }
    Ok(())
}

/// Every 15 min — log queue depth and retry items stuck in pending_retry.
pub async fn health_and_retry(state: &AppState, tx: &mpsc::Sender<IngestionJob>) {
    let depth = tx.max_capacity().saturating_sub(tx.capacity());
    tracing::info!(queue_depth = depth, "health check");

    let pending = match db::items::pending_retries(&state.pool, 20).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "fetching pending retries failed");
            return;
        }
    };

    for (id, _group_id, url) in pending {
        let fetched = if youtube::is_youtube(&url) {
            youtube::fetch(&url, &state.config.ytdlp_path).await
        } else {
            fetcher::fetch(&url).await
        };
        match fetched {
            Ok(content) if content.available => {
                let body = fetcher::truncate_for_llm(&content.text, 4000);
                let title = content.title.clone().unwrap_or_default();
                let (summary, tags, category) =
                    match state.chat.summarize(&title, &body).await {
                        Ok(s) => (s.summary, s.tags, s.category),
                        Err(_) => (content.text.chars().take(300).collect(), vec![], None),
                    };
                let embed_input = format!("{title}\n{summary}\n{}", tags.join(" "));
                let embedding = state.embedder.embed(&embed_input).await.ok();

                if let Err(e) = db::items::update_after_fetch(
                    &state.pool,
                    id,
                    content.title.as_deref(),
                    &content.text,
                    &summary,
                    &tags,
                    category.as_deref(),
                    embedding.as_deref(),
                )
                .await
                {
                    tracing::warn!(item_id = %id, error = %e, "retry update failed");
                } else {
                    tracing::info!(item_id = %id, "retry succeeded");
                }
            }
            _ => {
                // Still unavailable — stop retrying to avoid spamming the origin.
                let _ = db::items::mark_retry_exhausted(&state.pool, id).await;
            }
        }
    }
}
