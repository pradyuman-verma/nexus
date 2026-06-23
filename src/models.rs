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

/// A user's per-group interest profile.
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

/// A stored knowledge-graph item, as retrieved for query synthesis.
#[allow(dead_code)] // id/category/shared_by/similarity used by future ranking + dashboard
#[derive(Debug, Clone)]
pub struct RetrievedItem {
    pub id: Uuid,
    pub url: String,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub tags: Vec<String>,
    pub category: Option<String>,
    pub context_window: Option<ContextWindow>,
    pub shared_by: Option<i64>,
    pub shared_by_username: Option<String>,
    pub message_id: Option<i64>,
    pub shared_at: DateTime<Utc>,
    pub similarity: f32,
}
