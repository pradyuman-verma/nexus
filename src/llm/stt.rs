//! Speech-to-text via any OpenAI-compatible `/v1/audio/transcriptions`
//! endpoint (OpenAI whisper-1, Groq whisper-large-v3-turbo, …).
//! WhatsApp voice notes arrive as ogg/opus, which both accept.

use anyhow::{anyhow, Context, Result};
use reqwest::multipart::{Form, Part};
use serde::Deserialize;
use std::time::Duration;

pub struct Stt {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
}

#[derive(Deserialize)]
struct TranscriptionResponse {
    text: String,
}

impl Stt {
    pub fn new(base_url: String, api_key: String, model: String) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .expect("reqwest client");
        Self {
            http,
            base_url,
            api_key,
            model,
        }
    }

    pub async fn transcribe(&self, audio: Vec<u8>, mime: &str) -> Result<String> {
        // "audio/ogg; codecs=opus" → mime "audio/ogg", filename hint "ogg"
        let mime_base = mime.split(';').next().unwrap_or("audio/ogg").trim();
        let ext = mime_base.rsplit('/').next().unwrap_or("ogg");

        let part = Part::bytes(audio)
            .file_name(format!("voice.{ext}"))
            .mime_str(mime_base)
            .context("invalid audio mime type")?;
        let form = Form::new()
            .text("model", self.model.clone())
            .part("file", part);

        let resp = self
            .http
            .post(format!(
                "{}/audio/transcriptions",
                self.base_url.trim_end_matches('/')
            ))
            .bearer_auth(&self.api_key)
            .multipart(form)
            .send()
            .await
            .context("stt request failed")?;

        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!("stt api {status}: {text}"));
        }

        let parsed: TranscriptionResponse =
            serde_json::from_str(&text).context("decoding stt response")?;
        Ok(parsed.text.trim().to_string())
    }
}
