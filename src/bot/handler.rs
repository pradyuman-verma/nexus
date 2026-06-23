//! The single message endpoint: buffering, URL/forward detection, command and
//! @mention routing.

use crate::bot::commands;
use crate::db;
use crate::models::{ContextMessage, ContextPosition, ContextWindow, IngestionJob};
use crate::query;
use crate::state::AppState;
use std::time::Duration;
use teloxide::prelude::*;
use teloxide::types::ParseMode;

const CONTEXT_BEFORE: i64 = 3;
const CONTEXT_AFTER: i64 = 3;

/// teloxide endpoint. Always returns Ok — internal failures are logged, never
/// propagated (an error here would drop the update and spam logs).
pub async fn handle_message(_bot: Bot, msg: Message, state: AppState) -> ResponseResult<()> {
    if let Err(e) = handle_inner(&state, &msg).await {
        tracing::warn!(error = %e, "message handler error");
    }
    Ok(())
}

async fn handle_inner(state: &AppState, msg: &Message) -> anyhow::Result<()> {
    let chat_id = msg.chat.id.0;
    let message_id = msg.id.0 as i64;
    let text = msg.text().or_else(|| msg.caption()).unwrap_or("");

    let user = msg.from.as_ref();
    let user_id = user.map(|u| u.id.0 as i64);
    let username = user.and_then(|u| u.username.clone());
    let first_name = user.map(|u| u.first_name.clone());

    // 1. Persist group/user, buffer the message.
    db::groups::upsert_group(&state.pool, chat_id, msg.chat.title()).await?;
    if let Some(uid) = user_id {
        db::groups::upsert_user(&state.pool, uid, username.as_deref(), first_name.as_deref())
            .await?;
    }
    db::groups::buffer_message(
        &state.pool,
        chat_id,
        user_id,
        username.as_deref(),
        message_id,
        if text.is_empty() { None } else { Some(text) },
    )
    .await?;

    // 4. Command or @mention → query path (handled before URL ingestion so a
    //    "/ask https://..." isn't also ingested as a shared link).
    if let Some(uid) = user_id {
        if commands::try_handle(state, chat_id, uid, msg.id.0, text).await? {
            return Ok(());
        }
        if let Some(query) = mention_query(state, text) {
            let reply = query::handler::handle(state, chat_id, uid, &query).await?;
            state
                .bot
                .send_message(msg.chat.id, reply)
                .parse_mode(ParseMode::Html)
                .link_preview_options(crate::bot::formatter::no_preview())
                .reply_parameters(teloxide::types::ReplyParameters::new(msg.id))
                .await
                .ok();
            return Ok(());
        }
    }

    // 2/3. URL detection (incl. forwarded content).
    let urls = extract_urls(text);
    if urls.is_empty() {
        return Ok(());
    }
    let Some(uid) = user_id else { return Ok(()) };

    let forwarded = is_forwarded(msg);
    let forward_origin = forward_origin(msg);

    for url in urls {
        // Tier 1 dedup: skip t.me links and recently-seen URLs.
        if is_telegram_link(&url) {
            continue;
        }
        if db::items::is_duplicate(&state.pool, chat_id, &url, state.config.url_dedup_days)
            .await
            .unwrap_or(false)
        {
            tracing::debug!(%url, "skipping duplicate url");
            continue;
        }

        schedule_ingestion(
            state.clone(),
            url,
            chat_id,
            msg.chat.title().map(|s| s.to_string()),
            uid,
            message_id,
            forwarded,
            forward_origin.clone(),
        );
    }

    Ok(())
}

/// Build the context window (waiting for trailing messages) then enqueue.
#[allow(clippy::too_many_arguments)]
fn schedule_ingestion(
    state: AppState,
    url: String,
    group_id: i64,
    group_name: Option<String>,
    shared_by: i64,
    message_id: i64,
    forwarded: bool,
    forward_origin: Option<String>,
) {
    tokio::spawn(async move {
        let wait = state.config.context_window_wait_secs;
        tokio::time::sleep(Duration::from_secs(wait)).await;

        let context_window =
            build_context_window(&state, group_id, message_id, forwarded, forward_origin).await;

        let job = IngestionJob {
            url,
            group_id,
            group_name,
            shared_by,
            message_id,
            context_window,
        };
        if let Err(e) = state.ingestion_tx.send(job).await {
            tracing::error!(error = %e, "failed to enqueue ingestion job");
        }
    });
}

async fn build_context_window(
    state: &AppState,
    group_id: i64,
    message_id: i64,
    forwarded: bool,
    forward_origin: Option<String>,
) -> ContextWindow {
    let mut messages: Vec<ContextMessage> =
        db::groups::messages_before(&state.pool, group_id, message_id, CONTEXT_BEFORE)
            .await
            .unwrap_or_default();

    // Mark the pivot message (it's already in the buffer; pull it via before+1
    // is awkward, so just flag the link's own message by id boundary).
    if let Some(last) = messages.last_mut() {
        // no-op guard to keep `last` used if before-list is empty
        let _ = &last.message_id;
    }

    let after = db::groups::messages_after(&state.pool, group_id, message_id, CONTEXT_AFTER)
        .await
        .unwrap_or_default();
    messages.extend(after);

    // Ensure the pivot itself is represented.
    if !messages.iter().any(|m| m.message_id == message_id) {
        messages.push(ContextMessage {
            user_id: None,
            username: None,
            message_id,
            text: String::new(),
            position: ContextPosition::Pivot,
        });
        messages.sort_by_key(|m| m.message_id);
    }

    ContextWindow {
        messages,
        forwarded,
        forward_origin,
    }
}

// ── URL + forward helpers ───────────────────────────────────────────────────

/// Extract http(s) URLs from free text (whitespace-token scan + validation).
pub fn extract_urls(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for tok in text.split_whitespace() {
        let tok = tok.trim_matches(|c: char| {
            matches!(c, '(' | ')' | '[' | ']' | '<' | '>' | ',' | '"' | '\'' | '.' | '!' | '?')
        });
        if tok.starts_with("http://") || tok.starts_with("https://") {
            if url::Url::parse(tok).is_ok() && !out.contains(&tok.to_string()) {
                out.push(tok.to_string());
            }
        }
    }
    out
}

fn is_telegram_link(url: &str) -> bool {
    crate::bot::formatter::domain_of(url)
        .map(|d| d == "t.me" || d == "telegram.me" || d.ends_with(".t.me"))
        .unwrap_or(false)
}

fn is_forwarded(msg: &Message) -> bool {
    msg.forward_date().is_some()
}

fn forward_origin(msg: &Message) -> Option<String> {
    if let Some(chat) = msg.forward_from_chat() {
        return Some(chat.title().unwrap_or("forwarded chat").to_string());
    }
    if let Some(user) = msg.forward_from_user() {
        return Some(
            user.username
                .clone()
                .map(|u| format!("@{u}"))
                .unwrap_or_else(|| user.first_name.clone()),
        );
    }
    None
}

/// If the message @mentions the bot, return the query text with the mention removed.
fn mention_query(state: &AppState, text: &str) -> Option<String> {
    let handle = format!("@{}", state.bot_username);
    if text.to_lowercase().contains(&handle.to_lowercase()) {
        let stripped = text
            .split_whitespace()
            .filter(|t| !t.eq_ignore_ascii_case(&handle))
            .collect::<Vec<_>>()
            .join(" ");
        let stripped = stripped.trim().to_string();
        if stripped.is_empty() {
            None
        } else {
            Some(stripped)
        }
    } else {
        None
    }
}
