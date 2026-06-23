//! Shared, cheaply-clonable application state injected into every handler/task.

use crate::config::Config;
use crate::llm::chat::Chat;
use crate::llm::embeddings::Embedder;
use crate::models::IngestionJob;
use sqlx::PgPool;
use std::sync::Arc;
use teloxide::Bot;
use tokio::sync::mpsc;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub pool: PgPool,
    pub bot: Bot,
    pub chat: Arc<Chat>,
    pub embedder: Arc<Embedder>,
    /// The bot's own @username (lowercased, no '@'), for mention detection.
    pub bot_username: Arc<String>,
    /// Producer side of the ingestion queue (used by the message handler).
    pub ingestion_tx: mpsc::Sender<IngestionJob>,
}
