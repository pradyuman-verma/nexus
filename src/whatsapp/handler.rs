//! Per-message routing for the WhatsApp channel: resolve identity, then
//! capture (link/note/voice/photo) or answer (question) — all through the
//! same intake + query pipeline the Telegram channel uses.

use crate::state::AppState;
use crate::whatsapp::{format, types::Message};
use crate::{db, intake, query};
use anyhow::{anyhow, Result};
use chrono::Utc;

const CHANNEL: &str = "whatsapp";
const UNSUPPORTED: &str = "I can save links, text notes, voice notes, and photos for now — \
forward me one of those, or ask me a question about what you've saved.";

/// Entry point, spawned per inbound message. Never propagates errors —
/// the webhook already returned 200.
pub async fn process_message(state: AppState, msg: Message, profile_name: Option<String>) {
    if let Err(e) = process_inner(&state, &msg, profile_name).await {
        tracing::warn!(error = %e, msg_id = %msg.id, "whatsapp message failed");
    }
}

async fn process_inner(state: &AppState, msg: &Message, profile_name: Option<String>) -> Result<()> {
    let wa = state
        .wa
        .clone()
        .ok_or_else(|| anyhow!("whatsapp client not configured"))?;

    // Meta redelivers webhooks — first insert wins, replays are dropped.
    if !db::channels::record_event(&state.pool, CHANNEL, &msg.id).await? {
        tracing::debug!(msg_id = %msg.id, "duplicate delivery ignored");
        return Ok(());
    }

    // ── Identity: one personal space per WhatsApp sender ────────────────
    let display = profile_name.unwrap_or_else(|| msg.from.clone());
    let space_name = format!("WhatsApp · {display}");
    let group_id =
        db::channels::resolve_space(&state.pool, CHANNEL, &msg.from, Some(&space_name)).await?;
    let user_id =
        db::channels::resolve_user(&state.pool, CHANNEL, &msg.from, Some(&display)).await?;

    // WhatsApp message ids (wamids) are opaque strings; the buffer and
    // context windows need ordered i64s, so captures are keyed by arrival
    // time instead.
    let message_id = Utc::now().timestamp_millis();
    let forwarded = msg.was_forwarded();

    if let Err(e) = wa.mark_read(&msg.id).await {
        tracing::debug!(error = %e, "mark_read failed");
    }

    let ack_enabled = state
        .config
        .whatsapp
        .as_ref()
        .map(|w| w.ack_on_capture)
        .unwrap_or(false);
    let ack = |text: &'static str| {
        let wa = wa.clone();
        let to = msg.from.clone();
        async move {
            if ack_enabled {
                if let Err(e) = wa.send_text(&to, text).await {
                    tracing::warn!(error = %e, "capture ack failed");
                }
            }
        }
    };

    match msg.kind.as_str() {
        "text" => {
            let text = msg
                .text
                .as_ref()
                .map(|t| t.body.as_str())
                .unwrap_or("")
                .trim();
            if text.is_empty() {
                return Ok(());
            }
            db::groups::buffer_message(
                &state.pool,
                group_id,
                Some(user_id),
                Some(&display),
                message_id,
                Some(text),
            )
            .await?;

            let urls = intake::extract_urls(text);
            if !urls.is_empty() {
                let mut queued = false;
                for url in urls {
                    if db::items::is_duplicate(
                        &state.pool,
                        group_id,
                        &url,
                        state.config.url_dedup_days,
                    )
                    .await
                    .unwrap_or(false)
                    {
                        tracing::info!(%url, group_id, "link shared (duplicate — skipped)");
                        continue;
                    }
                    tracing::info!(%url, group_id, user = user_id, "whatsapp link queued");
                    intake::schedule_link(
                        state.clone(),
                        url,
                        group_id,
                        Some(space_name.clone()),
                        user_id,
                        message_id,
                        forwarded,
                        None,
                        "whatsapp",
                    );
                    queued = true;
                }
                if queued {
                    ack("✓ saved — reading it in the background").await;
                }
            } else if intake::is_question(text) {
                tracing::info!(group_id, user = user_id, "whatsapp query");
                let req = crate::bot::commands::parse_ask_args(group_id, text);
                let reply = query::handler::handle(
                    state,
                    group_id,
                    user_id,
                    query::QueryScope::Personal,
                    req.web,
                    query::QueryAnchor::default(),
                    &req.question,
                )
                .await?;
                wa.send_text(&msg.from, &format::html_to_plain(&reply.html)).await?;
            } else {
                intake::schedule_note(
                    state, group_id, Some(space_name), user_id, message_id, forwarded,
                    "note", text.to_string(), "whatsapp",
                )
                .await?;
                ack("✓ noted").await;
            }
        }
        "audio" => {
            let media = msg
                .audio
                .as_ref()
                .ok_or_else(|| anyhow!("audio message without audio payload"))?;
            let Some(stt) = state.stt.clone() else {
                wa.send_text(
                    &msg.from,
                    "Voice notes aren't enabled yet (no speech-to-text configured).",
                )
                .await?;
                return Ok(());
            };
            let (bytes, mime) = wa.download_media(&media.id).await?;
            let transcript = stt.transcribe(bytes, &mime).await?;
            if transcript.is_empty() {
                wa.send_text(&msg.from, "Couldn't hear anything in that voice note.")
                    .await?;
                return Ok(());
            }
            db::groups::buffer_message(
                &state.pool,
                group_id,
                Some(user_id),
                Some(&display),
                message_id,
                Some(&transcript),
            )
            .await?;
            intake::schedule_note(
                state, group_id, Some(space_name), user_id, message_id, forwarded,
                "voice", transcript, "whatsapp",
            )
            .await?;
            ack("✓ voice note transcribed & saved").await;
        }
        "image" => {
            let media = msg
                .image
                .as_ref()
                .ok_or_else(|| anyhow!("image message without image payload"))?;
            let (bytes, mime) = wa.download_media(&media.id).await?;
            let mime_base = mime.split(';').next().unwrap_or("image/jpeg").trim();
            let description = state.chat.describe_image(&bytes, mime_base).await?;
            let caption = media.caption.as_deref().unwrap_or("").trim();
            let text = if caption.is_empty() {
                description
            } else {
                format!("{caption}\n\n{description}")
            };
            db::groups::buffer_message(
                &state.pool,
                group_id,
                Some(user_id),
                Some(&display),
                message_id,
                Some(&text),
            )
            .await?;
            intake::schedule_note(
                state, group_id, Some(space_name), user_id, message_id, forwarded,
                "image", text, "whatsapp",
            )
            .await?;
            ack("✓ photo saved").await;
        }
        other => {
            tracing::info!(kind = other, "unsupported whatsapp message type");
            wa.send_text(&msg.from, UNSUPPORTED).await.ok();
        }
    }
    Ok(())
}

