//! Domain types shared across modules.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A single buffered message captured around a shared link.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextMessage {
    pub user_id: Option<i64>,
    pub username: Option<String>,
    pub message_id: i64,
    pub text: String,
    #[serde(default)]
    pub position: ContextPosition,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextPosition {
    Before,
    #[default]
    Pivot,
    After,
}

/// The conversational context surrounding a shared link — the moat.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ContextWindow {
    pub messages: Vec<ContextMessage>,
    #[serde(default)]
    pub forwarded: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forward_origin: Option<String>,
}

impl ContextWindow {
    /// Flatten the window to plain text for embedding / excerpt prompts.
    pub fn as_text(&self) -> String {
        self.messages
            .iter()
            .map(|m| {
                let who = m.username.as_deref().unwrap_or("someone");
                format!("{who}: {}", m.text)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// What the message handler hands to the ingestion queue.
#[derive(Debug, Clone)]
pub struct IngestionJob {
    pub url: String,
    pub group_id: i64,
    pub group_name: Option<String>,
    pub shared_by: i64,
    pub message_id: i64,
    pub context_window: ContextWindow,
    /// Ingress channel: telegram | whatsapp | x | manual.
    pub source_channel: String,
    /// Pre-extracted content (notes, voice transcripts, image descriptions).
    /// When set, Tier 1 fetch is skipped and `url` is a pseudo-URL
    /// (`note://…`, `voice://…`, `image://…`).
    pub note: Option<NoteContent>,
}

/// Channel-captured content that never had a URL to fetch.
#[derive(Debug, Clone)]
pub struct NoteContent {
    /// items.content_type value: 'note' | 'voice' | 'image'.
    pub content_type: String,
    pub title: Option<String>,
    pub text: String,
}

/// Result of fetching + extracting a URL (Tier 1).
#[allow(dead_code)] // author/published retained for future content_type handling
#[derive(Debug, Clone, Default)]
pub struct ExtractedContent {
    pub title: Option<String>,
    pub author: Option<String>,
    pub published: Option<String>,
    pub text: String,
    pub available: bool,
}

/// Output of Tier 2 summarization.
#[derive(Debug, Clone, Deserialize)]
pub struct Summary {
    pub summary: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub category: Option<String>,
}

/// A user's per-group interest profile (legacy — group-scoped notifications).
#[allow(dead_code)] // group_id kept for symmetry / future per-profile routing
#[derive(Debug, Clone)]
pub struct UserProfile {
    pub user_id: i64,
    pub group_id: i64,
    pub interest_vector: Option<Vec<f32>>,
    pub vector_weight: f32,
    pub relevance_threshold: f32,
    pub top_tags: Vec<String>,
    pub muted_until: Option<DateTime<Utc>>,
}

/// Global taste profile — one brain per user across all channels (Layer 2).
#[derive(Debug, Clone)]
pub struct UserTasteProfile {
    pub user_id: i64,
    pub interest_vector: Option<Vec<f32>>,
    pub vector_weight: f32,
    pub notify_threshold: f32,
    pub liked_tags: Vec<String>,
    pub disliked_tags: Vec<String>,
    pub capture_count: i32,
    pub query_count: i32,
    pub muted_until: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

/// Tier 2c structured signals extracted from a capture's context envelope.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContextSignals {
    #[serde(default)]
    pub intent: Option<String>,
    /// -1.0 (negative) to +1.0 (strong positive).
    #[serde(default)]
    pub sentiment: f32,
    #[serde(default)]
    pub entities: Vec<String>,
    #[serde(default = "default_signal_strength")]
    pub signal_strength: f32,
}

fn default_signal_strength() -> f32 {
    1.0
}

/// A stored knowledge-graph item, as retrieved for query synthesis.
#[allow(dead_code)] // id/category/shared_by/similarity used by future ranking + dashboard
#[derive(Debug, Clone)]
pub struct RetrievedItem {
    pub id: Uuid,
    pub url: String,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub raw_content: Option<String>,
    pub tags: Vec<String>,
    pub category: Option<String>,
    pub context_window: Option<ContextWindow>,
    pub shared_by: Option<i64>,
    pub shared_by_username: Option<String>,
    pub message_id: Option<i64>,
    pub shared_at: DateTime<Utc>,
    pub similarity: f32,
    /// Ingress channel: telegram | whatsapp | …
    pub source_channel: Option<String>,
    /// note | voice | image | article | …
    pub content_type: Option<String>,
    /// Telegram group / space where the item was captured.
    pub group_id: Option<i64>,
}
