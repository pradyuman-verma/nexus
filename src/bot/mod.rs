//! teloxide dispatcher setup.

pub mod commands;
pub mod formatter;
pub mod handler;

use crate::state::AppState;
use teloxide::prelude::*;

/// Run the bot dispatcher until shutdown. Blocks (awaits) for the process life.
pub async fn run(state: AppState) {
    let bot = state.bot.clone();
    let tree = Update::filter_message().endpoint(handler::handle_message);

    Dispatcher::builder(bot, tree)
        .dependencies(dptree::deps![state])
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;
}
