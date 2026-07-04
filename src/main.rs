//! Nexus — a group ambient intelligence layer for Telegram.
//!
//! Single binary: Telegram dispatcher, ingestion worker, relevance scorer,
//! query handler and cron scheduler all run in-process on one Tokio runtime.

use nexus::config::Config;
use nexus::llm::anthropic::Anthropic;
use nexus::llm::chat::Chat;
use nexus::llm::embeddings::Embedder;
use nexus::llm::ollama::Ollama;
use nexus::llm::stt::Stt;
use nexus::models::IngestionJob;
use nexus::state::AppState;
use nexus::search::TavilySearch;
use nexus::whatsapp::WhatsApp;
use nexus::{bot, cron, db, http, ingestion};
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
    let mut bot = Bot::new(&config.telegram_bot_token);
    if let Some(api_url) = &config.telegram_api_url {
        bot = bot.set_api_url(api_url.parse().context("parsing TELEGRAM_API_URL")?);
    }
    let me = bot.get_me().await.context("get_me failed — check TELEGRAM_BOT_TOKEN")?;
    let bot_username = me.username().to_string();
    tracing::info!(bot = %bot_username, "telegram connected");

    // ── Channel clients ─────────────────────────────────────────────────────
    let wa = config.whatsapp.as_ref().map(|w| {
        Arc::new(WhatsApp::new(
            w.graph_base_url.clone(),
            w.access_token.clone(),
            w.phone_number_id.clone(),
            w.api_version.clone(),
        ))
    });
    let stt = config.stt_api_key.clone().map(|key| {
        Arc::new(Stt::new(
            config.stt_base_url.clone(),
            key,
            config.stt_model.clone(),
        ))
    });
    let web_search = config.tavily_api_key.as_ref().map(|key| {
        Arc::new(TavilySearch::new(
            key.clone(),
            config.web_search_max_results,
        ))
    });

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
        wa,
        stt,
        web_search,
    };

    // Ingestion consumer (continuous background task).
    {
        let state = state.clone();
        tokio::spawn(async move { ingestion::run_consumer(state, rx).await });
    }

    // Webhook server — only when a webhook channel (WhatsApp) is configured.
    if state.config.whatsapp.is_some() {
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(e) = http::serve(state).await {
                tracing::error!(error = %e, "webhook server died — WhatsApp ingress is down");
            }
        });
        tracing::info!("whatsapp channel enabled");
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
