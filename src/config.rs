//! Typed configuration loaded from the environment (.env in dev).

use crate::llm::chat::ChatRoute;
use crate::llm::Provider;
use anyhow::{Context, Result};
use std::env;

#[derive(Debug, Clone)]
pub struct Config {
    pub telegram_bot_token: String,
    pub database_url: String,

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
    pub notification_score_log: bool,
    pub url_dedup_days: i64,
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
            database_url: req("DATABASE_URL")?,

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
            notification_score_log: opt("NOTIFICATION_SCORE_LOG", "true") == "true",
            url_dedup_days: opt("URL_DEDUP_DAYS", "7").parse().unwrap_or(7),
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

fn req(key: &str) -> Result<String> {
    env::var(key).with_context(|| format!("missing required env var {key}"))
}

fn opt(key: &str, default: &str) -> String {
    env::var(key)
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default.to_string())
}
