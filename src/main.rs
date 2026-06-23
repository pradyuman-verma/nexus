//! Nexus — a group ambient intelligence layer for Telegram.
//!
//! Single binary: Telegram dispatcher, ingestion worker, relevance scorer,
//! query handler and cron scheduler all run in-process on one Tokio runtime.

use nexus::config::Config;
use nexus::llm::anthropic::Anthropic;
use nexus::llm::chat::Chat;
use nexus::llm::embeddings::Embedder;
use nexus::llm::ollama::Ollama;
use nexus::models::IngestionJob;
use nexus::state::AppState;
use nexus::{bot, cron, db, ingestion};
use anyhow::{Context, Result};
use std::sync::Arc;
use teloxide::prelude::*;
use tokio::sync::mpsc;

const INGESTION_QUEUE_CAP: usize = 256;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    init_tracing();

    let config = Arc::new(Config::from_env()?);
    tracing::info!("starting Nexus");

    // ── Database ────────────────────────────────────────────────────────────
    let pool = db::init_pool(&config.database_url).await?;
    db::run_migrations(&pool).await?;
    db::ensure_vector_schema(&pool, config.embedding_dim).await?;
    tracing::info!(dim = config.embedding_dim, "database ready");

    // ── Model clients ───────────────────────────────────────────────────────
    let anthropic = config.anthropic_api_key.clone().map(|key| {
        Anthropic::new(key, config.haiku_model.clone(), config.sonnet_model.clone())
    });
    let ollama = if config.uses_ollama_chat() {
        Some(Ollama::new(
            config.ollama_base_url.clone(),
            config.ollama_chat_model.clone(),
            config.ollama_api_key.clone(),
        ))
    } else {
        None
    };
    let chat = Arc::new(Chat::new(anthropic, ollama, config.chat_route())?);

    let embedder = Arc::new(Embedder::new(
        config.embedding_url.clone(),
        config.embedding_api_key.clone(),
        config.embedding_model.clone(),
        config.embedding_dim,
    ));
    tracing::info!(
        model = %config.embedding_model,
        url = %config.embedding_url,
        "embedder ready"
    );

    // ── Telegram ────────────────────────────────────────────────────────────
    let bot = Bot::new(&config.telegram_bot_token);
    let me = bot.get_me().await.context("get_me failed — check TELEGRAM_BOT_TOKEN")?;
    let bot_username = me.username().to_string();
    tracing::info!(bot = %bot_username, "telegram connected");

    // ── Wiring ──────────────────────────────────────────────────────────────
    let (tx, rx) = mpsc::channel::<IngestionJob>(INGESTION_QUEUE_CAP);

    let state = AppState {
        config: config.clone(),
        pool: pool.clone(),
        bot: bot.clone(),
        chat,
        embedder,
        bot_username: Arc::new(bot_username.to_lowercase()),
        ingestion_tx: tx.clone(),
    };

    // Ingestion consumer (continuous background task).
    {
        let state = state.clone();
        tokio::spawn(async move { ingestion::run_consumer(state, rx).await });
    }

    // Cron scheduler. Keep the handle alive for the process lifetime.
    let _scheduler = cron::start(state.clone(), tx.clone()).await?;

    // ── Run the dispatcher (blocks until Ctrl-C) ───────────────────────────
    tracing::info!("Nexus is live");
    bot::run(state).await;

    tracing::info!("shutting down");
    Ok(())
}

fn init_tracing() {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("nexus=info,sqlx=warn"));
    tracing_subscriber::registry()
        .with(fmt::layer())
        .with(filter)
        .init();
}
