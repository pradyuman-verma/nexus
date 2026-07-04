//! WhatsApp Business Cloud API client — outbound messages and media download.

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use serde_json::json;
use std::time::Duration;

pub struct WhatsApp {
    http: reqwest::Client,
    base_url: String,
    access_token: String,
    phone_number_id: String,
    api_version: String,
}

#[derive(Deserialize)]
struct MediaInfo {
    url: String,
    #[serde(default)]
    mime_type: Option<String>,
}

impl WhatsApp {
    pub fn new(
        base_url: String,
        access_token: String,
        phone_number_id: String,
        api_version: String,
    ) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("reqwest client");
        Self {
            http,
            base_url,
            access_token,
            phone_number_id,
            api_version,
        }
    }

    fn graph(&self, path: &str) -> String {
        format!(
            "{}/{}/{path}",
            self.base_url.trim_end_matches('/'),
            self.api_version
        )
    }

    /// Free-form text reply. Only valid inside the 24h customer-service
    /// window, which is always open here because the user just messaged us.
    pub async fn send_text(&self, to: &str, body: &str) -> Result<()> {
        let url = self.graph(&format!("{}/messages", self.phone_number_id));
        let payload = json!({
            "messaging_product": "whatsapp",
            "to": to,
            "type": "text",
            "text": { "body": body, "preview_url": true },
        });
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.access_token)
            .json(&payload)
            .send()
            .await
            .context("whatsapp send request failed")?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("whatsapp send {status}: {text}"));
        }
        Ok(())
    }

    /// Blue-ticks the inbound message so the sender sees the bot is alive.
    /// Best-effort — failures are logged by callers, never fatal.
    pub async fn mark_read(&self, message_id: &str) -> Result<()> {
        let url = self.graph(&format!("{}/messages", self.phone_number_id));
        let payload = json!({
            "messaging_product": "whatsapp",
            "status": "read",
            "message_id": message_id,
        });
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.access_token)
            .json(&payload)
            .send()
            .await
            .context("whatsapp mark_read failed")?;
        if !resp.status().is_success() {
            return Err(anyhow!("whatsapp mark_read {}", resp.status()));
        }
        Ok(())
    }

    /// Resolve a media id to bytes. Two-step: GET /{media_id} returns a
    /// short-lived (~5 min) CDN url, then GET that url with the same token.
    pub async fn download_media(&self, media_id: &str) -> Result<(Vec<u8>, String)> {
        let info: MediaInfo = self
            .http
            .get(self.graph(media_id))
            .bearer_auth(&self.access_token)
            .send()
            .await
            .context("media info request failed")?
            .error_for_status()
            .context("media info request rejected")?
            .json()
            .await
            .context("decoding media info")?;

        let bytes = self
            .http
            .get(&info.url)
            .bearer_auth(&self.access_token)
            .send()
            .await
            .context("media download failed")?
            .error_for_status()
            .context("media download rejected")?
            .bytes()
            .await
            .context("reading media body")?;

        let mime = info
            .mime_type
            .unwrap_or_else(|| "application/octet-stream".into());
        Ok((bytes.to_vec(), mime))
    }
}
