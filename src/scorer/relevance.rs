//! The relevance interrupt — the core proactive feature.
//!
//! Runs immediately after every item is ingested: scores the item against every
//! other active user's interest vector and DMs a personal notification when the
//! score clears that user's threshold.

use crate::bot::formatter;
use crate::db;
use crate::scorer::vectors::cosine_similarity;
use crate::state::AppState;
use anyhow::Result;
use chrono::Utc;
use teloxide::prelude::*;
use teloxide::types::ParseMode;
use uuid::Uuid;

pub struct ScoredItem<'a> {
    pub item_id: Uuid,
    pub group_id: i64,
    pub sharer_id: i64,
    pub embedding: &'a [f32],
    pub title: &'a str,
    pub url: &'a str,
    pub message_id: i64,
    pub raw_content: Option<&'a str>,
}

/// Score one freshly-ingested item against the group and fire notifications.
/// Never returns an error to the caller path that would abort ingestion — all
/// per-user failures are logged and swallowed.
pub async fn run(state: &AppState, item: ScoredItem<'_>) {
    if let Err(e) = run_inner(state, &item).await {
        tracing::warn!(item_id = %item.item_id, error = %e, "relevance scorer failed");
    }
}

async fn run_inner(state: &AppState, item: &ScoredItem<'_>) -> Result<()> {
    let profiles =
        db::profiles::list_active_in_group(&state.pool, item.group_id, item.sharer_id).await?;
    if profiles.is_empty() {
        return Ok(());
    }

    let now = Utc::now();

    for profile in profiles {
        let Some(vec) = &profile.interest_vector else {
            continue;
        };
        let score = cosine_similarity(item.embedding, vec);

        let above = score >= profile.relevance_threshold;

        // Calibration logging for below-threshold scores.
        if !above {
            if state.config.notification_score_log {
                let _ =
                    db::notifications::log(&state.pool, profile.user_id, item.item_id, score, false)
                        .await;
            }
            continue;
        }

        // Respect mute.
        if profile.muted_until.map(|m| m > now).unwrap_or(false) {
            continue;
        }

        // Dedup.
        if db::notifications::already_notified(&state.pool, profile.user_id, item.item_id).await? {
            continue;
        }

        // Tier 2 excerpt tuned to this user's interests.
        let content = item.raw_content.unwrap_or(item.title);
        let excerpt = match state
            .chat
            .extract_excerpt(content, &profile.top_tags)
            .await
        {
            Ok(e) if !e.trim().is_empty() => e,
            _ => item.title.to_string(),
        };

        let sharer_name = db::groups::first_name(&state.pool, item.sharer_id)
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| "Someone".to_string());

        let link = formatter::message_link(item.group_id, item.message_id);
        let matching_tag = best_matching_tag(&profile.top_tags);

        let text = formatter::notification(
            &sharer_name,
            item.title,
            item.url,
            &excerpt,
            score,
            matching_tag,
            link.as_deref(),
        );

        // DM the user directly — never the group.
        match state
            .bot
            .send_message(ChatId(profile.user_id), text)
            .parse_mode(ParseMode::Html)
            .link_preview_options(formatter::no_preview())
            .await
        {
            Ok(_) => {
                db::notifications::log(&state.pool, profile.user_id, item.item_id, score, true)
                    .await?;
                // Receiving relevant content is a weak interest signal (0.5).
                let _ = db::profiles::apply_weighted_update(
                    &state.pool,
                    profile.user_id,
                    item.group_id,
                    item.embedding,
                    0.5,
                    state.config.max_vector_weight,
                    state.config.default_relevance_threshold,
                    &[],
                    false,
                )
                .await;
            }
            Err(e) => {
                // Most common cause: the user has never opened a DM with the bot.
                tracing::info!(user = profile.user_id, error = %e, "notification DM failed");
            }
        }
    }

    Ok(())
}

fn best_matching_tag(top_tags: &[String]) -> Option<&str> {
    top_tags.first().map(|s| s.as_str())
}
