//! Claude (Anthropic Messages API) low-level client.
//! Prompt construction lives in `llm::chat`; this is just transport.

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use serde_json::json;
use std::time::Duration;

const API_URL: &str = "https://api.anthropic.com/v1/messages";
const API_VERSION: &str = "2023-06-01";

pub struct Anthropic {
    http: reqwest::Client,
    api_key: String,
    pub haiku_model: String,
    pub sonnet_model: String,
}

#[derive(Deserialize)]
struct MessagesResponse {
    content: Vec<ContentBlock>,
}

#[derive(Deserialize)]
struct ContentBlock {
    #[serde(default)]
    text: String,
}

impl Anthropic {
    pub fn new(api_key: String, haiku_model: String, sonnet_model: String) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .expect("reqwest client");
        Self {
            http,
            api_key,
            haiku_model,
            sonnet_model,
        }
    }

    /// One Messages call with a single retry (2s backoff) on transient failure.
    pub async fn complete(
        &self,
        model: &str,
        system: Option<&str>,
        user: &str,
        max_tokens: u32,
    ) -> Result<String> {
        let mut body = json!({
            "model": model,
            "max_tokens": max_tokens,
            "messages": [{ "role": "user", "content": user }],
        });
        if let Some(s) = system {
            body["system"] = json!(s);
        }
        self.call(&body).await
    }

    /// Describe an image (base64 content block + instruction), on Haiku.
    pub async fn describe_image(&self, image: &[u8], media_type: &str, prompt: &str) -> Result<String> {
        use base64::{engine::general_purpose::STANDARD as B64, Engine};
        let body = json!({
            "model": &self.haiku_model,
            "max_tokens": 512,
            "messages": [{
                "role": "user",
                "content": [
                    {
                        "type": "image",
                        "source": {
                            "type": "base64",
                            "media_type": media_type,
                            "data": B64.encode(image),
                        }
                    },
                    { "type": "text", "text": prompt }
                ]
            }],
        });
        self.call(&body).await
    }

    async fn call(&self, body: &serde_json::Value) -> Result<String> {
        let mut last_err = None;
        for attempt in 0..2u8 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            match self.try_call(&body).await {
                Ok(text) => return Ok(text),
                Err(e) => {
                    tracing::warn!(attempt, error = %e, "anthropic call failed");
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow!("anthropic call failed")))
    }

    async fn try_call(&self, body: &serde_json::Value) -> Result<String> {
        let resp = self
            .http
            .post(API_URL)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .header("content-type", "application/json")
            .json(body)
            .send()
            .await
            .context("anthropic request failed")?;

        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!("anthropic {status}: {text}"));
        }

        let parsed: MessagesResponse =
            serde_json::from_str(&text).context("decoding anthropic response")?;
        Ok(parsed
            .content
            .into_iter()
            .map(|c| c.text)
            .collect::<Vec<_>>()
            .join("")
            .trim()
            .to_string())
    }
}
