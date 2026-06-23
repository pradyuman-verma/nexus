//! Model-tier definitions and LLM clients.
//!
//! Each task picks the cheapest model that can do the job:
//!
//! * Tier 1 — no model (URL detect, fetch, extraction, dedup)
//! * Tier 2 — Claude Haiku (summarize, tag, classify, excerpt)
//! * Tier 3 — embedding model (vectorize items/queries)
//! * Tier 4 — Claude Sonnet (entity + edge extraction)
//! * Tier 5 — Claude Sonnet with RAG context (Q&A synthesis)

pub mod anthropic;
pub mod chat;
pub mod embeddings;
pub mod ollama;

/// Which backend serves a given chat tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Anthropic,
    Ollama,
}

impl Provider {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "ollama" | "local" => Provider::Ollama,
            _ => Provider::Anthropic,
        }
    }
}

/// Documents the tiering scheme; dispatch is implicit at each call site today.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelTier {
    /// No model — deterministic local work.
    None,
    /// Claude Haiku — cheap summarization / extraction.
    Haiku,
    /// Embedding model.
    Embed,
    /// Claude Sonnet — graph extraction.
    SonnetGraph,
    /// Claude Sonnet — RAG synthesis.
    SonnetRag,
}
