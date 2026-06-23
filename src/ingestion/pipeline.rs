//! Orchestrates a single ingestion job: Tier 1 → 2 → 3 → DB → scorer.

use crate::bot::formatter::domain_of;
use crate::db::{self, items::NewItem};
use crate::ingestion::{fetcher, youtube};
use crate::models::IngestionJob;
use crate::scorer::relevance::{self, ScoredItem};
use crate::state::AppState;
use anyhow::Result;

const SUMMARIZE_TOKEN_BUDGET: usize = 4000;

pub async fn process(state: &AppState, job: IngestionJob) {
    if let Err(e) = process_inner(state, job).await {
        // Ingestion failures MUST NOT crash the bot.
        tracing::error!(error = %e, "ingestion job failed");
    }
}

async fn process_inner(state: &AppState, job: IngestionJob) -> Result<()> {
    // Ensure FK targets exist before inserting the item.
    db::groups::upsert_group(&state.pool, job.group_id, job.group_name.as_deref()).await?;

    let domain = domain_of(&job.url).unwrap_or_else(|| job.url.clone());

    // ── Tier 1: classify + fetch + extract ──────────────────────────────────
    // YouTube links go through the transcript fetcher; everything else is HTML.
    let is_video = youtube::is_youtube(&job.url);
    let content_type = if is_video { "video" } else { "article" };

    let fetch_result = if is_video {
        youtube::fetch(&job.url, &state.config.ytdlp_path).await
    } else {
        fetcher::fetch(&job.url).await
    };
    let extracted = match fetch_result {
        Ok(c) => c,
        Err(e) => {
            tracing::info!(url = %job.url, content_type, error = %e, "fetch failed; storing stub");
            Default::default()
        }
    };

    let available = extracted.available;
    let title = extracted
        .title
        .clone()
        .unwrap_or_else(|| domain.clone());

    // ── Tier 2: summarize (only when we have real content) ─────────────────
    let (summary, tags, category, fetch_status) = if available {
        let body = fetcher::truncate_for_llm(&extracted.text, SUMMARIZE_TOKEN_BUDGET);
        match state.chat.summarize(&title, &body).await {
            Ok(s) => (s.summary, s.tags, s.category, "ok"),
            Err(e) => {
                tracing::warn!(error = %e, "summarize failed");
                (
                    truncate_summary(&extracted.text),
                    Vec::new(),
                    None,
                    "ok",
                )
            }
        }
    } else {
        (
            format!("Content unavailable — {domain}"),
            Vec::new(),
            None,
            "pending_retry",
        )
    };

    // ── Tier 3: embed the distilled signal ─────────────────────────────────
    let embed_input = if available {
        format!("{title}\n{summary}\n{}", tags.join(" "))
    } else {
        // Still useful for graph edges + notifications: URL, domain, and the
        // surrounding conversation.
        format!("{title}\n{domain}\n{}", job.context_window.as_text())
    };

    let embedding = match state.embedder.embed(&embed_input).await {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!(error = %e, "embedding failed; storing item without vector");
            None
        }
    };

    // ── Persist ─────────────────────────────────────────────────────────────
    let raw_content = if available {
        Some(extracted.text.as_str())
    } else {
        None
    };

    let item_id = db::items::insert(
        &state.pool,
        NewItem {
            group_id: job.group_id,
            shared_by: job.shared_by,
            url: &job.url,
            message_id: job.message_id,
            title: Some(&title),
            raw_content,
            summary: Some(&summary),
            tags: &tags,
            category: category.as_deref(),
            context_window: &job.context_window,
            embedding: embedding.as_deref(),
            fetch_status,
            content_type,
        },
    )
    .await?;

    let Some(item_id) = item_id else {
        tracing::info!(url = %job.url, "item deduped on insert");
        return Ok(());
    };

    tracing::info!(item_id = %item_id, url = %job.url, available, "item ingested");

    let Some(embedding) = embedding else {
        // No vector → no relevance scoring or profile update possible.
        return Ok(());
    };

    // ── Sharer profile update: sharing is a strong intent signal (2.0) ──────
    db::profiles::apply_weighted_update(
        &state.pool,
        job.shared_by,
        job.group_id,
        &embedding,
        2.0,
        state.config.max_vector_weight,
        state.config.default_relevance_threshold,
        &tags,
        true, // create profile from an explicit share
    )
    .await?;

    // ── Relevance interrupt against everyone else ──────────────────────────
    relevance::run(
        state,
        ScoredItem {
            item_id,
            group_id: job.group_id,
            sharer_id: job.shared_by,
            embedding: &embedding,
            title: &title,
            url: &job.url,
            message_id: job.message_id,
            raw_content,
        },
    )
    .await;

    Ok(())
}

fn truncate_summary(text: &str) -> String {
    let s: String = text.chars().take(300).collect();
    if text.chars().count() > 300 {
        format!("{s}…")
    } else {
        s
    }
}
