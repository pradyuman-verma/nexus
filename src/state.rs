//! Shared, cheaply-clonable application state injected into every handler/task.

use crate::config::Config;
use crate::llm::chat::Chat;
use crate::llm::embeddings::Embedder;
use crate::llm::stt::Stt;
use crate::models::IngestionJob;
use crate::whatsapp::WhatsApp;
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
    /// WhatsApp Cloud API client — None unless the channel is configured.
    pub wa: Option<Arc<WhatsApp>>,
    /// Speech-to-text for voice note captures — None without STT_API_KEY.
    pub stt: Option<Arc<Stt>>,
}
