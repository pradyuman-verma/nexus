//! Typed configuration loaded from the environment (.env in dev).

use crate::llm::chat::ChatRoute;
use crate::llm::Provider;
use anyhow::{Context, Result};
use std::env;

/// WhatsApp Cloud API channel — present only when the WA_* env vars are set.
#[derive(Debug, Clone)]
pub struct WhatsAppConfig {
    /// Permanent system-user token with whatsapp_business_messaging scope.
    pub access_token: String,
    /// The sender phone number id (NOT the display number).
    pub phone_number_id: String,
    /// Meta app secret — signs every webhook POST (X-Hub-Signature-256).
    pub app_secret: String,
    /// Arbitrary string; must match what you type into the Meta webhook UI.
    pub verify_token: String,
    pub api_version: String,
    /// Meta Graph API root — overridable so local dev can point at a stub.
    pub graph_base_url: String,
    /// Reply "✓ saved" after each capture. Nice while testing; can go quiet.
    pub ack_on_capture: bool,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub telegram_bot_token: String,
    /// Override for local testing against a stub Telegram API; None = real.
    pub telegram_api_url: Option<String>,
    /// React 👀 or reply "✓ saved" after each Telegram capture.
    pub tg_ack_on_capture: bool,
    pub database_url: String,

    // ── Webhook channels (WhatsApp now, IG/Twitter later) ───────────────
    pub whatsapp: Option<WhatsAppConfig>,
    /// Port for the webhook HTTP server (behind Caddy for TLS).
    pub http_port: u16,

    // ── Speech-to-text (voice note captures) ────────────────────────────
    /// Any OpenAI-compatible /v1/audio/transcriptions endpoint.
    pub stt_base_url: String,
    pub stt_api_key: Option<String>,
    pub stt_model: String,

    // ── Chat (Tiers 2/4/5) ──────────────────────────────────────────────
    /// Optional — only required if any tier routes to Anthropic.
    pub anthropic_api_key: Option<String>,
    pub haiku_model: String,
    pub sonnet_model: String,
    /// OpenAI-compatible chat base, e.g. `http://localhost:11434/v1` (Ollama)
    /// or `https://api.groq.com/openai/v1` (Groq), `https://api.deepseek.com` (DeepSeek).
    pub ollama_base_url: String,
    pub ollama_chat_model: String,
    /// None for local Ollama; set for hosted providers (Groq/DeepSeek/…).
    pub ollama_api_key: Option<String>,
    pub tier2_provider: Provider,
    pub graph_provider: Provider, // tier 4
    pub rag_provider: Provider,   // tier 5

    // ── Embeddings (Tier 3) ─────────────────────────────────────────────
    pub embedding_url: String,
    pub embedding_api_key: Option<String>,
    pub embedding_model: String,
    pub embedding_dim: usize,

    // ── Ingestion ───────────────────────────────────────────────────────
    /// Path to the yt-dlp binary used for YouTube transcript fetching.
    pub ytdlp_path: String,

    // ── Behaviour ───────────────────────────────────────────────────────
    pub context_window_wait_secs: u64,
    pub ingestion_batch_size: usize,
    pub graph_cron_schedule: String,
    pub cleanup_cron_schedule: String,
    pub health_cron_schedule: String,
    pub default_relevance_threshold: f32,
    pub max_vector_weight: f32,
    /// Exponential decay on taste vector weight (per day). 0 disables decay.
    pub taste_decay_lambda: f32,
    pub notification_score_log: bool,
    pub url_dedup_days: i64,

    // ── Web search (/ask --web) ───────────────────────────────────────────
    /// Tavily API key — enables `--web` augmentation when set.
    pub tavily_api_key: Option<String>,
    pub web_search_max_results: usize,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let ollama_base_url = opt("OLLAMA_BASE_URL", "http://localhost:11434/v1");

        // Embeddings default to a local Ollama model (nomic-embed-text, 768-dim)
        // so the bot runs with no OpenAI account. Override for OpenAI/Voyage.
        let embedding_url = opt(
            "EMBEDDING_BASE_URL",
            &format!("{ollama_base_url}/embeddings"),
        );
        // An API key is only needed for hosted providers (OpenAI/Voyage).
        let embedding_api_key = env::var("EMBEDDING_API_KEY")
            .ok()
            .or_else(|| env::var("OPENAI_API_KEY").ok())
            .filter(|s| !s.is_empty());

        Ok(Self {
            telegram_bot_token: req("TELEGRAM_BOT_TOKEN")?,
            telegram_api_url: env::var("TELEGRAM_API_URL").ok().filter(|s| !s.is_empty()),
            tg_ack_on_capture: opt("TG_ACK_ON_CAPTURE", "true") == "true",
            database_url: req("DATABASE_URL")?,

            whatsapp: whatsapp_from_env()?,
            http_port: opt("PORT", "8080").parse().unwrap_or(8080),

            stt_base_url: opt("STT_BASE_URL", "https://api.groq.com/openai/v1"),
            stt_api_key: env::var("STT_API_KEY").ok().filter(|s| !s.is_empty()),
            stt_model: opt("STT_MODEL", "whisper-large-v3-turbo"),

            anthropic_api_key: env::var("ANTHROPIC_API_KEY").ok().filter(|s| !s.is_empty()),
            haiku_model: opt("HAIKU_MODEL", "claude-haiku-4-5-20251001"),
            sonnet_model: opt("SONNET_MODEL", "claude-sonnet-4-6"),
            ollama_base_url,
            ollama_chat_model: opt("OLLAMA_CHAT_MODEL", "qwen2.5:3b-instruct"),
            ollama_api_key: env::var("OLLAMA_API_KEY").ok().filter(|s| !s.is_empty()),
            tier2_provider: Provider::parse(&opt("TIER2_PROVIDER", "anthropic")),
            graph_provider: Provider::parse(&opt("GRAPH_PROVIDER", "anthropic")),
            rag_provider: Provider::parse(&opt("RAG_PROVIDER", "anthropic")),

            embedding_url,
            embedding_api_key,
            embedding_model: opt("EMBEDDING_MODEL", "nomic-embed-text"),
            embedding_dim: opt("EMBEDDING_DIM", "768").parse().unwrap_or(768),

            ytdlp_path: opt("YTDLP_PATH", "yt-dlp"),

            context_window_wait_secs: opt("CONTEXT_WINDOW_WAIT_SECS", "60").parse().unwrap_or(60),
            ingestion_batch_size: opt("INGESTION_BATCH_SIZE", "20").parse().unwrap_or(20),
            graph_cron_schedule: opt("GRAPH_CRON_SCHEDULE", "0 0 */6 * * *"),
            cleanup_cron_schedule: opt("CLEANUP_CRON_SCHEDULE", "0 0 0 * * *"),
            health_cron_schedule: opt("HEALTH_CRON_SCHEDULE", "0 */15 * * * *"),
            default_relevance_threshold: opt("DEFAULT_RELEVANCE_THRESHOLD", "0.72")
                .parse()
                .unwrap_or(0.72),
            max_vector_weight: opt("MAX_VECTOR_WEIGHT", "100.0").parse().unwrap_or(100.0),
            taste_decay_lambda: opt("TASTE_DECAY_LAMBDA", "0.02").parse().unwrap_or(0.02),
            notification_score_log: opt("NOTIFICATION_SCORE_LOG", "true") == "true",
            url_dedup_days: opt("URL_DEDUP_DAYS", "7").parse().unwrap_or(7),

            tavily_api_key: env::var("TAVILY_API_KEY").ok().filter(|s| !s.is_empty()),
            web_search_max_results: opt("WEB_SEARCH_MAX_RESULTS", "5")
                .parse()
                .unwrap_or(5),
        })
    }

    pub fn chat_route(&self) -> ChatRoute {
        ChatRoute {
            tier2: self.tier2_provider,
            tier4: self.graph_provider,
            tier5: self.rag_provider,
        }
    }

    /// True if any chat tier needs a local Ollama chat model.
    pub fn uses_ollama_chat(&self) -> bool {
        [self.tier2_provider, self.graph_provider, self.rag_provider]
            .iter()
            .any(|p| *p == Provider::Ollama)
    }
}

/// All four WA_* vars present → Some; none present → None; a partial set
/// is a config mistake and fails fast.
fn whatsapp_from_env() -> Result<Option<WhatsAppConfig>> {
    const KEYS: [&str; 4] = [
        "WA_ACCESS_TOKEN",
        "WA_PHONE_NUMBER_ID",
        "WA_APP_SECRET",
        "WA_VERIFY_TOKEN",
    ];
    let set: Vec<&str> = KEYS
        .iter()
        .copied()
        .filter(|k| env::var(k).map(|v| !v.is_empty()).unwrap_or(false))
        .collect();
    if set.is_empty() {
        return Ok(None);
    }
    if set.len() < KEYS.len() {
        anyhow::bail!(
            "partial WhatsApp config: {} set but all of {} are required",
            set.join(", "),
            KEYS.join(", ")
        );
    }
    Ok(Some(WhatsAppConfig {
        access_token: req("WA_ACCESS_TOKEN")?,
        phone_number_id: req("WA_PHONE_NUMBER_ID")?,
        app_secret: req("WA_APP_SECRET")?,
        verify_token: req("WA_VERIFY_TOKEN")?,
        api_version: opt("WA_API_VERSION", "v20.0"),
        graph_base_url: opt("GRAPH_BASE_URL", "https://graph.facebook.com"),
        ack_on_capture: opt("WA_ACK_ON_CAPTURE", "true") == "true",
    }))
}

fn req(key: &str) -> Result<String> {
    env::var(key).with_context(|| format!("missing required env var {key}"))
}

fn opt(key: &str, default: &str) -> String {
    env::var(key)
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default.to_string())
}
