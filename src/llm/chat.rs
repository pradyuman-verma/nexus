//! Tiered chat tasks (summarize, excerpt, graph, synthesis) with per-tier
//! provider routing between Claude and a local Ollama model.

use crate::llm::anthropic::Anthropic;
use crate::llm::ollama::Ollama;
use crate::llm::Provider;
use crate::models::Summary;
use anyhow::{anyhow, Context, Result};

/// Which provider serves each chat tier. Defaults can be set per tier in config.
#[derive(Debug, Clone, Copy)]
pub struct ChatRoute {
    pub tier2: Provider, // summarize + excerpt
    pub tier4: Provider, // graph extraction
    pub tier5: Provider, // RAG synthesis
}

pub struct Chat {
    anthropic: Option<Anthropic>,
    ollama: Option<Ollama>,
    route: ChatRoute,
}

impl Chat {
    pub fn new(
        anthropic: Option<Anthropic>,
        ollama: Option<Ollama>,
        route: ChatRoute,
    ) -> Result<Self> {
        // Validate that every routed provider is actually configured.
        for (tier, p) in [
            ("tier2", route.tier2),
            ("tier4", route.tier4),
            ("tier5", route.tier5),
        ] {
            match p {
                Provider::Anthropic if anthropic.is_none() => {
                    return Err(anyhow!(
                        "{tier} routed to anthropic but ANTHROPIC_API_KEY is not set"
                    ));
                }
                Provider::Ollama if ollama.is_none() => {
                    return Err(anyhow!(
                        "{tier} routed to ollama but Ollama is not configured"
                    ));
                }
                _ => {}
            }
        }
        Ok(Self {
            anthropic,
            ollama,
            route,
        })
    }

    /// Anthropic model id to use for a tier (Haiku for 2, Sonnet for 4/5).
    fn anthropic_model<'a>(a: &'a Anthropic, tier: Tier) -> &'a str {
        match tier {
            Tier::Two => &a.haiku_model,
            Tier::Four | Tier::Five => &a.sonnet_model,
        }
    }

    async fn complete(
        &self,
        tier: Tier,
        system: Option<&str>,
        user: &str,
        max_tokens: u32,
    ) -> Result<String> {
        let provider = match tier {
            Tier::Two => self.route.tier2,
            Tier::Four => self.route.tier4,
            Tier::Five => self.route.tier5,
        };
        match provider {
            Provider::Anthropic => {
                let a = self
                    .anthropic
                    .as_ref()
                    .ok_or_else(|| anyhow!("anthropic unset"))?;
                let model = Self::anthropic_model(a, tier);
                a.complete(model, system, user, max_tokens).await
            }
            Provider::Ollama => {
                let o = self
                    .ollama
                    .as_ref()
                    .ok_or_else(|| anyhow!("ollama unset"))?;
                o.complete(system, user, max_tokens).await
            }
        }
    }

    // ── Tier 2: summarize + tag + classify ──────────────────────────────────
    pub async fn summarize(&self, title: &str, content: &str) -> Result<Summary> {
        let user = format!(
            "Title: {title}\n\nContent:\n{content}\n\n\
             Summarize this content in 3 sentences. Extract 5 topic tags \
             (lowercase, single words or short phrases). Classify into one category. \
             Return JSON only, no prose: \
             {{\"summary\": \"...\", \"tags\": [\"...\"], \"category\": \"...\"}}"
        );
        let raw = self.complete(Tier::Two, None, &user, 1024).await?;
        parse_json(&raw).context("parsing summary JSON")
    }

    // ── Tier 2: relevant excerpt for a notification ─────────────────────────
    pub async fn extract_excerpt(&self, content: &str, interests: &[String]) -> Result<String> {
        let interest_list = if interests.is_empty() {
            "general technology and startups".to_string()
        } else {
            interests.join(", ")
        };
        let user = format!(
            "Article:\n{content}\n\n\
             A user's interests: [{interest_list}]. \
             Extract the 1-2 sentences from the article most relevant to those \
             interests. Return only those sentences, no preamble."
        );
        self.complete(Tier::Two, None, &user, 256).await
    }

    // ── Query expansion (HyDE + multi-query) for retrieval ─────────────────
    pub async fn expand_query(&self, question: &str) -> Result<QueryExpansion> {
        let user = format!(
            "User question: {question}\n\n\
             Generate retrieval aids to find documents that answer it. \
             Return JSON only: \
             {{\"probes\": [\"2-3 alternative phrasings or focused sub-questions\"], \
             \"hyde\": \"1-3 sentences of a plausible hypothetical answer such a document might contain\"}}"
        );
        let raw = self.complete(Tier::Two, None, &user, 400).await?;
        parse_json(&raw).context("parsing query expansion")
    }

    // ── Tier 4: entity + edge extraction ────────────────────────────────────
    pub async fn extract_graph(&self, batch_prompt: &str) -> Result<String> {
        self.complete(Tier::Four, None, batch_prompt, 4096).await
    }

    // ── Tier 5: RAG synthesis ───────────────────────────────────────────────
    pub async fn synthesize(&self, system: &str, context_and_question: &str) -> Result<String> {
        self.complete(Tier::Five, Some(system), context_and_question, 2000)
            .await
    }
}

#[derive(Clone, Copy)]
enum Tier {
    Two,
    Four,
    Five,
}

/// Retrieval probes generated from a user question (HyDE + multi-query).
#[derive(serde::Deserialize, Default)]
pub struct QueryExpansion {
    #[serde(default)]
    pub probes: Vec<String>,
    #[serde(default)]
    pub hyde: String,
}

/// Tolerantly pull a JSON object out of an LLM response (handles ```json fences).
fn parse_json<T: serde::de::DeserializeOwned>(raw: &str) -> Result<T> {
    let cleaned = raw.trim();
    let cleaned = cleaned
        .strip_prefix("```json")
        .or_else(|| cleaned.strip_prefix("```"))
        .unwrap_or(cleaned)
        .trim_end_matches("```")
        .trim();

    if let Ok(v) = serde_json::from_str::<T>(cleaned) {
        return Ok(v);
    }
    let start = cleaned.find('{');
    let end = cleaned.rfind('}');
    if let (Some(s), Some(e)) = (start, end) {
        if e > s {
            return serde_json::from_str::<T>(&cleaned[s..=e])
                .context("no valid JSON object found");
        }
    }
    Err(anyhow!("no JSON object in response: {cleaned}"))
}
