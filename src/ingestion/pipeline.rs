//! Orchestrates a single ingestion job: Tier 1 → 2 → 2c → 3 → DB → Layer 2 taste.

use crate::bot::formatter::domain_of;
use crate::db::{self, events::event_type, items::NewItem};
use crate::ingestion::{chunker, fetcher, youtube};
use crate::models::{ContextSignals, IngestionJob};
use crate::scorer::relevance::{self, ScoredItem};
use crate::state::AppState;
use anyhow::Result;
use uuid::Uuid;

const SUMMARIZE_TOKEN_BUDGET: usize = 4000;
const DEFAULT_CAPTURE_SIGNAL: f32 = 2.0;

pub async fn process(state: &AppState, job: IngestionJob) {
    if let Err(e) = process_inner(state, job).await {
        tracing::error!(error = %e, "ingestion job failed");
    }
}

async fn process_inner(state: &AppState, job: IngestionJob) -> Result<()> {
    db::groups::upsert_group(&state.pool, job.group_id, job.group_name.as_deref()).await?;

    let owner_user_id = job.shared_by;
    let domain = domain_of(&job.url).unwrap_or_else(|| job.url.clone());

    // ── Tier 1: classify + fetch + extract ──────────────────────────────────
    let (content_type, extracted) = if let Some(note) = &job.note {
        (
            note.content_type.as_str(),
            crate::models::ExtractedContent {
                title: note.title.clone(),
                author: None,
                published: None,
                text: note.text.clone(),
                available: true,
            },
        )
    } else {
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
        (content_type, extracted)
    };

    let available = extracted.available;
    let title = extracted.title.clone().unwrap_or_else(|| domain.clone());

    // ── Tier 2: summarize (only when we have real content) ─────────────────
    let (summary, tags, category, fetch_status) = if available {
        let body = fetcher::truncate_for_llm(&extracted.text, SUMMARIZE_TOKEN_BUDGET);
        match state.chat.summarize(&title, &body).await {
            Ok(s) => (s.summary, s.tags, s.category, "ok"),
            Err(e) => {
                tracing::warn!(error = %e, "summarize failed");
                (truncate_summary(&extracted.text), Vec::new(), None, "ok")
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

    // ── Tier 2c: parse context envelope → structured user signals ───────────
    let context_signals = parse_context_signals(state, &job, &title).await;

    // ── Tier 3: embed distilled signal + context intent ─────────────────────
    let embed_input = build_embed_input(
        &title,
        &summary,
        &tags,
        &domain,
        &job.context_window.as_text(),
        &context_signals,
        available,
    );

    let embedding = match state.embedder.embed(&embed_input).await {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!(error = %e, "embedding failed; storing item without vector");
            None
        }
    };

    // ── Persist capture (Layer 1) ───────────────────────────────────────────
    let raw_content = if available {
        Some(extracted.text.as_str())
    } else {
        None
    };

    let item_id = db::items::insert(
        &state.pool,
        NewItem {
            group_id: job.group_id,
            owner_user_id,
            shared_by: job.shared_by,
            source_channel: &job.source_channel,
            url: &job.url,
            message_id: job.message_id,
            title: Some(&title),
            raw_content,
            summary: Some(&summary),
            tags: &tags,
            category: category.as_deref(),
            context_window: &job.context_window,
            context_signals: Some(&context_signals),
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

    tracing::info!(
        item_id = %item_id,
        url = %job.url,
        owner = owner_user_id,
        channel = %job.source_channel,
        available,
        "capture ingested"
    );

    let body = if available {
        extracted.text.as_str()
    } else {
        summary.as_str()
    };
    if let Err(e) = index_chunks(state, item_id, job.group_id, owner_user_id, &title, body).await {
        tracing::warn!(item_id = %item_id, error = %e, "chunk indexing failed");
    }

    // ── Layer 2: log event + update global taste profile ────────────────────
    let capture_signal = context_signals.signal_strength.max(DEFAULT_CAPTURE_SIGNAL);
    let _ = db::events::log(
        &state.pool,
        owner_user_id,
        event_type::CAPTURE,
        Some(item_id),
        capture_signal,
        serde_json::json!({
            "channel": job.source_channel,
            "url": job.url,
            "intent": context_signals.intent,
            "sentiment": context_signals.sentiment,
            "tags": tags,
        }),
    )
    .await;

    if let Some(ref embedding) = embedding {
        db::taste::apply_signal(
            &state.pool,
            owner_user_id,
            embedding,
            capture_signal,
            state.config.max_vector_weight,
            state.config.default_relevance_threshold,
            state.config.taste_decay_lambda,
            &tags,
            true,
            true,
            false,
        )
        .await?;

        relevance::run(
            state,
            ScoredItem {
                item_id,
                group_id: job.group_id,
                sharer_id: job.shared_by,
                embedding,
                title: &title,
                url: &job.url,
                message_id: job.message_id,
                raw_content,
                tags: &tags,
                shared_at: chrono::Utc::now(),
            },
        )
        .await;
    }

    Ok(())
}

async fn parse_context_signals(
    state: &AppState,
    job: &IngestionJob,
    title: &str,
) -> ContextSignals {
    let context_text = job.context_window.as_text();
    let user_comment = pivot_comment(&job.context_window);
    match state
        .chat
        .parse_context_signals(title, &context_text, &user_comment)
        .await
    {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!(error = %e, "tier 2c context parse failed; using defaults");
            ContextSignals {
                intent: if job.context_window.forwarded {
                    Some("forwarded share".into())
                } else {
                    Some("saved link".into())
                },
                sentiment: 0.5,
                entities: Vec::new(),
                signal_strength: DEFAULT_CAPTURE_SIGNAL,
            }
        }
    }
}

fn pivot_comment(ctx: &crate::models::ContextWindow) -> String {
    ctx.messages
        .iter()
        .find(|m| m.position == crate::models::ContextPosition::Pivot)
        .map(|m| m.text.clone())
        .unwrap_or_default()
}

fn build_embed_input(
    title: &str,
    summary: &str,
    tags: &[String],
    domain: &str,
    context_text: &str,
    signals: &ContextSignals,
    available: bool,
) -> String {
    if available {
        let intent = signals.intent.as_deref().unwrap_or("");
        format!("{title}\n{summary}\n{}\nintent: {intent}", tags.join(" "))
    } else {
        format!("{title}\n{domain}\n{context_text}")
    }
}

pub async fn index_chunks(
    state: &AppState,
    item_id: Uuid,
    group_id: i64,
    owner_user_id: i64,
    title: &str,
    body: &str,
) -> Result<usize> {
    let passages = chunker::chunk_text(body);
    if passages.is_empty() {
        return Ok(0);
    }

    let mut rows: Vec<(i32, String, Vec<f32>)> = Vec::with_capacity(passages.len());
    for (i, passage) in passages.into_iter().enumerate() {
        let embed_input = format!("{title}\n{passage}");
        match state.embedder.embed(&embed_input).await {
            Ok(vec) => rows.push((i as i32, passage, vec)),
            Err(e) => {
                tracing::warn!(item_id = %item_id, chunk = i, error = %e, "chunk embed failed")
            }
        }
    }

    if rows.is_empty() {
        return Ok(0);
    }
    let n = rows.len();
    db::chunks::replace_for_item(&state.pool, item_id, group_id, owner_user_id, &rows).await?;
    Ok(n)
}

fn truncate_summary(text: &str) -> String {
    let s: String = text.chars().take(300).collect();
    if text.chars().count() > 300 {
        format!("{s}…")
    } else {
        s
    }
}
