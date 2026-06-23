//! Tier 3 embedding client for any OpenAI-compatible `/v1/embeddings` endpoint.
//! Works unchanged against OpenAI, a local Ollama server (`nomic-embed-text`,
//! `mxbai-embed-large`, …), or Voyage — only `base_url`, `api_key` and `model`
//! differ.

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use serde_json::json;
use std::time::Duration;

pub struct Embedder {
    http: reqwest::Client,
    /// Full embeddings URL, e.g. `https://api.openai.com/v1/embeddings`
    /// or `http://localhost:11434/v1/embeddings`.
    url: String,
    /// None for local servers that don't require auth.
    api_key: Option<String>,
    model: String,
    dim: usize,
}

#[derive(Deserialize)]
struct EmbedResponse {
    data: Vec<EmbedData>,
}

#[derive(Deserialize)]
struct EmbedData {
    embedding: Vec<f32>,
}

impl Embedder {
    pub fn new(url: String, api_key: Option<String>, model: String, dim: usize) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .expect("reqwest client");
        Self {
            http,
            url,
            api_key,
            model,
            dim,
        }
    }

    #[allow(dead_code)] // exposed for callers that validate dimensions
    pub fn dim(&self) -> usize {
        self.dim
    }

    /// Embed a single string. Input is truncated defensively to keep request
    /// size bounded; callers should pass distilled signal, not full articles.
    pub async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let input = truncate_chars(text, 8000);
        let body = json!({ "model": self.model, "input": input });

        let mut last_err = None;
        for attempt in 0..2u8 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            match self.try_embed(&body).await {
                Ok(v) => return Ok(v),
                Err(e) => {
                    tracing::warn!(attempt, error = %e, "embedding call failed");
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow!("embedding failed")))
    }

    async fn try_embed(&self, body: &serde_json::Value) -> Result<Vec<f32>> {
        let mut req = self.http.post(&self.url).json(body);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }
        let resp = req.send().await.context("embedding request failed")?;

        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!("embedding api {status}: {text}"));
        }

        let parsed: EmbedResponse =
            serde_json::from_str(&text).context("decoding embedding response")?;
        let vec = parsed
            .data
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("empty embedding response"))?
            .embedding;

        if vec.len() != self.dim {
            return Err(anyhow!(
                "embedding dim mismatch: got {}, expected {}",
                vec.len(),
                self.dim
            ));
        }
        Ok(vec)
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}
