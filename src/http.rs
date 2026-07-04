//! HTTP surface for webhook-based channels (currently WhatsApp).
//! Only started when a webhook channel is configured; Telegram remains
//! long-polling and needs no inbound HTTP.
//!
//! The POST path only verifies, parses, and spawns — every slow thing
//! (media download, STT, LLM calls) happens off the request path.

use crate::state::AppState;
use crate::whatsapp::{handler, signature, types::WebhookPayload};
use anyhow::{Context, Result};
use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use std::collections::HashMap;

pub async fn serve(state: AppState) -> Result<()> {
    let port = state.config.http_port;
    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route(
            "/webhook/whatsapp",
            get(verify_webhook).post(receive_webhook),
        )
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port))
        .await
        .context("binding webhook listener")?;
    tracing::info!(port, "webhook server listening");
    axum::serve(listener, app).await.context("webhook server")
}

/// Meta's one-time subscription handshake: echo hub.challenge back iff
/// the verify token matches.
async fn verify_webhook(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let expected = state.config.whatsapp.as_ref().map(|w| w.verify_token.as_str());
    let mode_ok = params.get("hub.mode").map(String::as_str) == Some("subscribe");
    let token_ok = params.get("hub.verify_token").map(String::as_str) == expected && expected.is_some();

    match (mode_ok && token_ok, params.get("hub.challenge")) {
        (true, Some(challenge)) => (StatusCode::OK, challenge.clone()),
        _ => {
            tracing::warn!("webhook verification rejected");
            (StatusCode::FORBIDDEN, String::new())
        }
    }
}

/// Inbound webhook. Signature failures get 401; everything after that
/// returns 200 no matter what — Meta disables webhooks that keep failing,
/// and redeliveries are already deduped on the message id.
async fn receive_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> StatusCode {
    let Some(wa_config) = state.config.whatsapp.as_ref() else {
        return StatusCode::NOT_FOUND;
    };
    let sig = headers
        .get("x-hub-signature-256")
        .and_then(|v| v.to_str().ok());
    if !signature::verify(&wa_config.app_secret, &body, sig) {
        tracing::warn!("webhook signature verification failed");
        return StatusCode::UNAUTHORIZED;
    }

    let payload: WebhookPayload = match serde_json::from_slice(&body) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "unparseable webhook payload");
            return StatusCode::OK;
        }
    };

    for entry in payload.entry {
        for change in entry.changes {
            let value = change.value;
            for message in &value.messages {
                let name = value.profile_name(&message.from);
                // Spawn per message: the webhook must return immediately.
                tokio::spawn(handler::process_message(
                    state.clone(),
                    message.clone(),
                    name,
                ));
            }
        }
    }
    StatusCode::OK
}
