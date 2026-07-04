//! Inline-keyboard callback handling — taste feedback on /ask and relevance DMs.

use crate::db::{self, events::event_type};
use crate::state::AppState;
use teloxide::prelude::*;
use teloxide::types::InlineKeyboardMarkup;
use uuid::Uuid;

/// teloxide endpoint for callback queries.
pub async fn handle_callback(_bot: Bot, q: CallbackQuery, state: AppState) -> ResponseResult<()> {
    if let Err(e) = handle_inner(&state, &q).await {
        tracing::warn!(error = %e, "callback handler error");
    }
    Ok(())
}

async fn handle_inner(state: &AppState, q: &CallbackQuery) -> anyhow::Result<()> {
    let Some(data) = q.data.as_deref() else {
        return Ok(());
    };
    let user_id = q.from.id.0 as i64;

    let toast = match data {
        "au" => {
            handle_ask_up(state, user_id).await?;
            "Thanks!"
        }
        d if d.starts_with("ad:") => {
            let id = parse_uuid(d.strip_prefix("ad:").unwrap_or(""))?;
            handle_ask_down(state, user_id, id).await?;
            "Got it — I'll tune down similar results."
        }
        d if d.starts_with("nd:") => {
            let id = parse_uuid(d.strip_prefix("nd:").unwrap_or(""))?;
            handle_notify_dismiss(state, user_id, id).await?
        }
        _ => return Ok(()),
    };

    state.bot.answer_callback_query(q.id.clone()).text(toast).await?;

    if let Some(message) = q.regular_message() {
        let _ = state
            .bot
            .edit_message_reply_markup(message.chat.id, message.id)
            .reply_markup(InlineKeyboardMarkup::default())
            .await;
    }

    Ok(())
}

async fn handle_ask_up(state: &AppState, user_id: i64) -> anyhow::Result<()> {
    let _ = db::events::log(
        &state.pool,
        user_id,
        event_type::ASK_THUMBS_UP,
        None,
        1.0,
        serde_json::json!({}),
    )
    .await;
    Ok(())
}

async fn handle_ask_down(state: &AppState, user_id: i64, item_id: Uuid) -> anyhow::Result<()> {
    let _ = db::events::log(
        &state.pool,
        user_id,
        event_type::ASK_THUMBS_DOWN,
        Some(item_id),
        -1.0,
        serde_json::json!({}),
    )
    .await;

    if let Some((embedding, tags)) = db::items::signal_source(&state.pool, item_id).await? {
        let _ = db::taste::apply_signal(
            &state.pool,
            user_id,
            &embedding,
            -1.0,
            state.config.max_vector_weight,
            state.config.default_relevance_threshold,
            state.config.taste_decay_lambda,
            &tags,
            true,
            false,
            false,
        )
        .await;
    }
    Ok(())
}

async fn handle_notify_dismiss(
    state: &AppState,
    user_id: i64,
    item_id: Uuid,
) -> anyhow::Result<&'static str> {
    let _ = db::events::log(
        &state.pool,
        user_id,
        event_type::NOTIFY_DISMISS,
        Some(item_id),
        -1.0,
        serde_json::json!({}),
    )
    .await;

    if let Some((embedding, tags)) = db::items::signal_source(&state.pool, item_id).await? {
        let _ = db::taste::apply_signal(
            &state.pool,
            user_id,
            &embedding,
            -1.0,
            state.config.max_vector_weight,
            state.config.default_relevance_threshold,
            state.config.taste_decay_lambda,
            &tags,
            true,
            false,
            false,
        )
        .await;
    }

    let toast = if db::taste::calibrate_threshold(
        &state.pool,
        user_id,
        state.config.default_relevance_threshold,
        3,
        0.05,
    )
    .await?
    .is_some()
    {
        "Marked not relevant — I'll ping you less often."
    } else {
        "Marked not relevant."
    };

    Ok(toast)
}

fn parse_uuid(s: &str) -> anyhow::Result<Uuid> {
    Uuid::parse_str(s).map_err(|e| anyhow::anyhow!("invalid callback uuid: {e}"))
}
