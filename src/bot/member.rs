//! Group membership updates — privacy-mode nudge when the bot joins.

use crate::state::AppState;
use teloxide::prelude::*;

pub const PRIVACY_NUDGE: &str = "Tip: disable privacy mode in BotFather (Bot Settings → \
Group Privacy → Turn off) so I can see shared links.";

/// Send the privacy nudge once when the bot is added to a group.
pub async fn handle_my_chat_member(
    _bot: Bot,
    update: ChatMemberUpdated,
    state: AppState,
) -> ResponseResult<()> {
    if let Err(e) = handle_inner(&state, &update).await {
        tracing::warn!(error = %e, "my_chat_member handler error");
    }
    Ok(())
}

async fn handle_inner(state: &AppState, update: &ChatMemberUpdated) -> anyhow::Result<()> {
    if update.chat.is_private() {
        return Ok(());
    }
    if !update.new_chat_member.is_present() || update.old_chat_member.is_present() {
        return Ok(());
    }

    tracing::info!(chat_id = update.chat.id.0, "bot added to group");
    send_privacy_nudge(state, update.chat.id).await;
    Ok(())
}

pub async fn send_privacy_nudge(state: &AppState, chat_id: ChatId) {
    if let Err(e) = state.bot.send_message(chat_id, PRIVACY_NUDGE).await {
        tracing::debug!(error = %e, "privacy nudge failed");
    }
}
