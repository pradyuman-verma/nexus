//! OpenAI-compatible chat client (`/v1/chat/completions`).
//!
//! Despite the name, this drives any OpenAI-compatible chat endpoint — a local
//! Ollama server (no key) or a hosted OSS provider like Groq / DeepSeek /
//! Together / OpenRouter (Bearer key). Embeddings use the separate `Embedder`,
//! which is also OpenAI-compatible.

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use serde_json::json;
use std::time::Duration;

pub struct Ollama {
    http: reqwest::Client,
    /// Base like `http://localhost:11434/v1` (no trailing slash).
    base_url: String,
    model: String,
    /// None for local servers; set for hosted providers that require auth.
    api_key: Option<String>,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: ChatMessage,
}

#[derive(Deserialize)]
struct ChatMessage {
    #[serde(default)]
    content: String,
}

impl Ollama {
    pub fn new(base_url: String, model: String, api_key: Option<String>) -> Self {
        // Local CPU generation can be slow; give it room.
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(180))
            .build()
            .expect("reqwest client");
        Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
            model,
            api_key,
        }
    }

    pub async fn complete(
        &self,
        system: Option<&str>,
        user: &str,
        max_tokens: u32,
    ) -> Result<String> {
        let mut messages = Vec::new();
        if let Some(s) = system {
            messages.push(json!({ "role": "system", "content": s }));
        }
        messages.push(json!({ "role": "user", "content": user }));

        let body = json!({
            "model": self.model,
            "messages": messages,
            "max_tokens": max_tokens,
            "stream": false,
        });

        let mut last_err = None;
        for attempt in 0..2u8 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            match self.try_call(&body).await {
                Ok(text) => return Ok(text),
                Err(e) => {
                    tracing::warn!(attempt, error = %e, "ollama call failed");
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow!("ollama call failed")))
    }

    async fn try_call(&self, body: &serde_json::Value) -> Result<String> {
        let url = format!("{}/chat/completions", self.base_url);
        let mut req = self.http.post(&url).json(body);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }
        let resp = req.send().await.context("chat request failed")?;

        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!("ollama {status}: {text}"));
        }

        let parsed: ChatResponse =
            serde_json::from_str(&text).context("decoding ollama response")?;
        Ok(parsed
            .choices
            .into_iter()
            .next()
            .map(|c| c.message.content)
            .unwrap_or_default()
            .trim()
            .to_string())
    }
}
