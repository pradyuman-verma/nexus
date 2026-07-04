//! The single message endpoint: buffering, URL/forward detection, command and
//! capture routing (links, notes, voice, photos) and @mention queries.
//!
//! ## Group vs DM behaviour
//! - **DMs**: passive capture — links, text notes, voice, and photos are saved
//!   without @mentioning. Questions (heuristic) route to `/ask`.
//! - **Groups**: links in any message are captured passively (existing behaviour).
//!   Text notes, voice, and photos require an @mention of the bot so we don't
//!   ingest ambient group chatter. @mention + question → query; @mention + note → capture.

use crate::bot::commands;
use crate::db;
use crate::intake;
use crate::query;
use crate::state::AppState;
use teloxide::net::Download;
use teloxide::prelude::*;
use teloxide::types::ReactionType;

const CHANNEL: &str = "telegram";

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

    tracing::debug!(
        chat_id,
        user = ?username.as_deref().or(first_name.as_deref()),
        message_id,
        len = text.chars().count(),
        "rx message"
    );

    let in_private = msg.chat.is_private();
    let mentioned_text = mention_query(state, text);
    let capture_eligible = in_private || mentioned_text.is_some();

    // 1. Persist group/user, buffer the message.
    db::groups::upsert_group(&state.pool, chat_id, msg.chat.title()).await?;
    if let Some(uid) = user_id {
        db::groups::upsert_user(&state.pool, uid, username.as_deref(), first_name.as_deref())
            .await?;
    }

    buffer_reply_parent(state, chat_id, msg).await?;

    let forwarded = is_forwarded(msg);
    let forward_origin = forward_origin(msg);

    // Voice note — DMs or @mention in groups.
    if msg.voice().is_some() {
        if !capture_eligible {
            return Ok(());
        }
        let Some(uid) = user_id else {
            return Ok(());
        };
        return handle_voice(state, msg, chat_id, uid, message_id, forwarded).await;
    }

    // Photo — DMs or @mention in groups.
    if msg.photo().is_some() {
        let Some(uid) = user_id else {
            return Ok(());
        };
        // URLs in a photo caption are captured passively even without @mention.
        if !text.is_empty() {
            let queued = schedule_urls(
                state,
                msg,
                chat_id,
                uid,
                message_id,
                text,
                forwarded,
                forward_origin.clone(),
            )
            .await?;
            if queued {
                capture_ack(state, msg, "✓ saved — reading it in the background").await;
            }
        }
        if !capture_eligible {
            return Ok(());
        }
        return handle_photo(state, msg, chat_id, uid, message_id, forwarded).await;
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

    // 2. Slash commands (text only).
    if let Some(uid) = user_id {
        if commands::try_handle(state, chat_id, uid, msg.id.0, text, in_private).await? {
            return Ok(());
        }
    }

    // 3. Shared links — passive in groups and DMs.
    let urls = intake::extract_urls(text);
    if !urls.is_empty() {
        let Some(uid) = user_id else {
            return Ok(());
        };
        if !in_private {
            maybe_privacy_nudge(state, msg, chat_id).await?;
        }
        let queued = schedule_urls(
            state,
            msg,
            chat_id,
            uid,
            message_id,
            text,
            forwarded,
            forward_origin,
        )
        .await?;
        if queued {
            capture_ack(state, msg, "✓ saved — reading it in the background").await;
        }
        return Ok(());
    }

    // 4. Text notes and @mention queries — DMs or @mention in groups only.
    if !capture_eligible || text.trim().is_empty() {
        return Ok(());
    }
    let Some(uid) = user_id else {
        return Ok(());
    };

    let body = mentioned_text.as_deref().unwrap_or(text).trim();
    if body.is_empty() {
        return Ok(());
    }

    let (scope, question) = commands::parse_ask_args(chat_id, body);
    let wants_query =
        matches!(scope, query::QueryScope::Group(_)) || intake::is_question(&question);

    if wants_query {
        if question.is_empty() {
            return Ok(());
        }
        tracing::info!(chat_id, user = uid, "telegram query");
        let reply = commands::run_query(state, chat_id, uid, scope, &question).await?;
        commands::send_query_reply(state, chat_id, msg.id.0, &reply).await;
    } else {
        tracing::info!(chat_id, user = uid, "telegram note queued");
        intake::schedule_note(
            state,
            chat_id,
            msg.chat.title().map(|s| s.to_string()),
            uid,
            message_id,
            forwarded,
            "note",
            body.to_string(),
            CHANNEL,
        )
        .await?;
        capture_ack(state, msg, "✓ noted").await;
    }

    Ok(())
}

async fn handle_voice(
    state: &AppState,
    msg: &Message,
    chat_id: i64,
    uid: i64,
    message_id: i64,
    forwarded: bool,
) -> anyhow::Result<()> {
    let voice = msg.voice().expect("voice branch");
    let username = msg.from.as_ref().and_then(|u| u.username.as_deref());

    let Some(stt) = state.stt.clone() else {
        state
            .bot
            .send_message(
                msg.chat.id,
                "Voice notes aren't enabled yet (no speech-to-text configured).",
            )
            .reply_parameters(teloxide::types::ReplyParameters::new(msg.id))
            .await
            .ok();
        return Ok(());
    };

    let (bytes, mime) = download_voice(state, voice).await?;
    let transcript = stt.transcribe(bytes, &mime).await?;
    if transcript.is_empty() {
        state
            .bot
            .send_message(msg.chat.id, "Couldn't hear anything in that voice note.")
            .reply_parameters(teloxide::types::ReplyParameters::new(msg.id))
            .await
            .ok();
        return Ok(());
    }

    db::groups::buffer_message(
        &state.pool,
        chat_id,
        Some(uid),
        username,
        message_id,
        Some(&transcript),
    )
    .await?;

    tracing::info!(chat_id, user = uid, "telegram voice queued");
    intake::schedule_note(
        state,
        chat_id,
        msg.chat.title().map(|s| s.to_string()),
        uid,
        message_id,
        forwarded,
        "voice",
        transcript,
        CHANNEL,
    )
    .await?;
    capture_ack(state, msg, "✓ voice note transcribed & saved").await;
    Ok(())
}

async fn handle_photo(
    state: &AppState,
    msg: &Message,
    chat_id: i64,
    uid: i64,
    message_id: i64,
    forwarded: bool,
) -> anyhow::Result<()> {
    let photos = msg.photo().expect("photo branch");
    let username = msg.from.as_ref().and_then(|u| u.username.as_deref());
    let largest = photos.last().expect("photo message has sizes");

    let bytes = download_file(state, &largest.file.id).await?;
    let description = state.chat.describe_image(&bytes, "image/jpeg").await?;
    let caption = msg.caption().unwrap_or("").trim();
    let text = if caption.is_empty() {
        description
    } else {
        format!("{caption}\n\n{description}")
    };

    db::groups::buffer_message(
        &state.pool,
        chat_id,
        Some(uid),
        username,
        message_id,
        Some(&text),
    )
    .await?;

    tracing::info!(chat_id, user = uid, "telegram photo queued");
    intake::schedule_note(
        state,
        chat_id,
        msg.chat.title().map(|s| s.to_string()),
        uid,
        message_id,
        forwarded,
        "image",
        text,
        CHANNEL,
    )
    .await?;
    capture_ack(state, msg, "✓ photo saved").await;
    Ok(())
}

/// Enqueue every non-duplicate, non-t.me URL in `text`. Returns true if at
/// least one link was queued.
async fn schedule_urls(
    state: &AppState,
    msg: &Message,
    chat_id: i64,
    uid: i64,
    message_id: i64,
    text: &str,
    forwarded: bool,
    forward_origin: Option<String>,
) -> anyhow::Result<bool> {
    let urls = intake::extract_urls(text);
    let mut queued = false;
    for url in urls {
        if is_telegram_link(&url) {
            continue;
        }
        if db::items::is_duplicate(&state.pool, chat_id, &url, state.config.url_dedup_days)
            .await
            .unwrap_or(false)
        {
            tracing::info!(%url, chat_id, "link shared (duplicate — skipped)");
            continue;
        }
        tracing::info!(%url, chat_id, user = uid, "link shared — queued for ingestion");
        intake::schedule_link(
            state.clone(),
            url,
            chat_id,
            msg.chat.title().map(|s| s.to_string()),
            uid,
            message_id,
            forwarded,
            forward_origin.clone(),
            CHANNEL,
        );
        queued = true;
    }
    Ok(queued)
}

async fn download_file(state: &AppState, file_id: &str) -> anyhow::Result<Vec<u8>> {
    let file = state.bot.get_file(file_id).await?;
    let mut buf = Vec::new();
    state.bot.download_file(&file.path, &mut buf).await?;
    Ok(buf)
}

async fn download_voice(state: &AppState, voice: &teloxide::types::Voice) -> anyhow::Result<(Vec<u8>, String)> {
    let bytes = download_file(state, &voice.file.id).await?;
    let mime = voice
        .mime_type
        .as_ref()
        .map(|m| m.to_string())
        .unwrap_or_else(|| "audio/ogg".to_string());
    Ok((bytes, mime))
}

async fn buffer_reply_parent(state: &AppState, chat_id: i64, msg: &Message) -> anyhow::Result<()> {
    let Some(parent) = msg.reply_to_message() else {
        return Ok(());
    };
    let text = parent.text().or_else(|| parent.caption()).unwrap_or("");
    if text.is_empty() {
        return Ok(());
    }
    let user = parent.from.as_ref();
    let uid = user.map(|u| u.id.0 as i64);
    let uname = user.and_then(|u| u.username.as_deref());
    if let Some(uid) = uid {
        db::groups::upsert_user(
            &state.pool,
            uid,
            uname,
            user.map(|u| u.first_name.as_str()),
        )
        .await?;
    }
    db::groups::buffer_message(
        &state.pool,
        chat_id,
        uid,
        uname,
        parent.id.0 as i64,
        Some(text),
    )
    .await?;
    Ok(())
}

async fn maybe_privacy_nudge(state: &AppState, msg: &Message, chat_id: i64) -> anyhow::Result<()> {
    let count = db::groups::buffer_count(&state.pool, chat_id).await.unwrap_or(0);
    if count == 0 {
        crate::bot::member::send_privacy_nudge(state, msg.chat.id).await;
    }
    Ok(())
}

async fn capture_ack(state: &AppState, msg: &Message, text: &str) {
    if !state.config.tg_ack_on_capture {
        return;
    }
    let reacted = state
        .bot
        .set_message_reaction(msg.chat.id, msg.id)
        .reaction(vec![ReactionType::Emoji {
            emoji: "👀".to_string(),
        }])
        .await
        .is_ok();
    if reacted {
        return;
    }
    if let Err(e) = state
        .bot
        .send_message(msg.chat.id, text)
        .reply_parameters(teloxide::types::ReplyParameters::new(msg.id))
        .await
    {
        tracing::warn!(error = %e, "capture ack failed");
    }
}

// ── Forward helpers ─────────────────────────────────────────────────────────

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn telegram_links_skipped() {
        assert!(is_telegram_link("https://t.me/nexus/123"));
        assert!(is_telegram_link("https://telegram.me/foo"));
        assert!(!is_telegram_link("https://example.com"));
    }
}
