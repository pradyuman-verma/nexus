//! teloxide dispatcher setup.

pub mod callbacks;
pub mod commands;
pub mod formatter;
pub mod handler;
pub mod member;

use crate::state::AppState;
use teloxide::prelude::*;
use teloxide::types::BotCommand;

async fn register_commands(bot: &Bot) {
    let commands = vec![
        BotCommand {
            command: "ask".into(),
            description: "Search your personal brain".into(),
        },
        BotCommand {
            command: "stats".into(),
            description: "Captures and taste profile".into(),
        },
        BotCommand {
            command: "taste".into(),
            description: "Liked/disliked tags and threshold".into(),
        },
        BotCommand {
            command: "threshold".into(),
            description: "Tune relevance DM sensitivity".into(),
        },
        BotCommand {
            command: "mute".into(),
            description: "Pause relevance DMs for 24h".into(),
        },
        BotCommand {
            command: "help".into(),
            description: "Commands and usage".into(),
        },
    ];
    if let Err(e) = bot.set_my_commands(commands).await {
        tracing::warn!(error = %e, "set_my_commands failed");
    }
}

/// Run the bot dispatcher until shutdown. Blocks (awaits) for the process life.
pub async fn run(state: AppState) {
    let bot = state.bot.clone();
    register_commands(&bot).await;

    let handler = dptree::entry()
        .branch(Update::filter_message().endpoint(handler::handle_message))
        .branch(Update::filter_callback_query().endpoint(callbacks::handle_callback))
        .branch(Update::filter_my_chat_member().endpoint(member::handle_my_chat_member));

    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![state])
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;
}
