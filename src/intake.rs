//! Channel-agnostic intake: every ingress (Telegram, WhatsApp, later
//! IG/Twitter) funnels captures through here into the one ingestion queue.
//! Channels resolve identities and transport; intake owns URL extraction,
//! context windows, and job scheduling.

use crate::db;
use crate::models::{ContextMessage, ContextPosition, ContextWindow, IngestionJob, NoteContent};
use crate::state::AppState;
use std::time::Duration;

const CONTEXT_BEFORE: i64 = 3;
const CONTEXT_AFTER: i64 = 3;

/// Does a URL-free message read like a question about the brain rather
/// than a note to capture? Cheap heuristic: interrogative opener or "?".
pub fn is_question(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() {
        return false;
    }
    if t.ends_with('?') {
        return true;
    }
    let first = t.split_whitespace().next().unwrap_or("").to_lowercase();
    const OPENERS: &[&str] = &[
        "what",
        "whats",
        "what's",
        "where",
        "which",
        "who",
        "when",
        "how",
        "why",
        "find",
        "show",
        "search",
        "any",
        "got",
        "do",
        "does",
        "did",
        "is",
        "are",
        "was",
        "were",
        "can",
        "could",
        "recommend",
        "suggest",
        "list",
    ];
    OPENERS.contains(&first.as_str())
}

/// Question refers to something just shared ("this link", "what do you think")?
pub fn is_deictic_query(text: &str) -> bool {
    let t = text.trim().to_lowercase();
    if t.is_empty() {
        return false;
    }
    const PHRASES: &[&str] = &[
        "this",
        "that",
        "it",
        "the link",
        "the article",
        "the post",
        "the video",
        "the note",
        "the photo",
        "what i just",
        "what i shared",
        "what we just",
        "just shared",
        "just sent",
    ];
    PHRASES.iter().any(|p| t.contains(p))
}

/// Extract http(s) URLs from free text (whitespace-token scan + validation).
pub fn extract_urls(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for tok in text.split_whitespace() {
        let tok = tok.trim_matches(|c: char| {
            matches!(
                c,
                '(' | ')' | '[' | ']' | '<' | '>' | ',' | '"' | '\'' | '.' | '!' | '?'
            )
        });
        if (tok.starts_with("http://") || tok.starts_with("https://"))
            && url::Url::parse(tok).is_ok()
            && !out.contains(&tok.to_string())
        {
            out.push(tok.to_string());
        }
    }
    out
}

/// Schedule a shared link: wait for trailing chatter, build the context
/// window from the message buffer, then enqueue.
#[allow(clippy::too_many_arguments)]
pub fn schedule_link(
    state: AppState,
    url: String,
    group_id: i64,
    group_name: Option<String>,
    shared_by: i64,
    message_id: i64,
    forwarded: bool,
    forward_origin: Option<String>,
    source_channel: &str,
) {
    let source_channel = source_channel.to_string();
    tokio::spawn(async move {
        let wait = state.config.context_window_wait_secs;
        tokio::time::sleep(Duration::from_secs(wait)).await;

        let context_window =
            build_context_window(&state, group_id, message_id, forwarded, forward_origin).await;

        let job = IngestionJob {
            url,
            group_id,
            group_name,
            shared_by,
            message_id,
            context_window,
            source_channel,
            note: None,
        };
        if let Err(e) = state.ingestion_tx.send(job).await {
            tracing::error!(error = %e, "failed to enqueue ingestion job");
        }
    });
}

/// Enqueue pre-extracted content (a text note, voice transcript, or image
/// description). No fetch, no wait — the content is already in hand.
#[allow(clippy::too_many_arguments)]
pub async fn schedule_note(
    state: &AppState,
    group_id: i64,
    group_name: Option<String>,
    shared_by: i64,
    message_id: i64,
    forwarded: bool,
    content_type: &str,
    text: String,
    source_channel: &str,
) -> anyhow::Result<()> {
    let title = derive_title(&text, content_type);
    let context_window = build_context_window(state, group_id, message_id, forwarded, None).await;

    let job = IngestionJob {
        // Pseudo-URL: unique per capture, satisfies items.url NOT NULL and
        // the (group, url, day) dedup index without colliding.
        url: format!("{content_type}://{group_id}/{message_id}"),
        group_id,
        group_name,
        shared_by,
        message_id,
        context_window,
        source_channel: source_channel.to_string(),
        note: Some(NoteContent {
            content_type: content_type.to_string(),
            title: Some(title),
            text,
        }),
    };
    state
        .ingestion_tx
        .send(job)
        .await
        .map_err(|e| anyhow::anyhow!("failed to enqueue note job: {e}"))
}

/// Build the context window around a pivot message from the rolling buffer.
pub async fn build_context_window(
    state: &AppState,
    group_id: i64,
    message_id: i64,
    forwarded: bool,
    forward_origin: Option<String>,
) -> ContextWindow {
    let mut messages: Vec<ContextMessage> =
        db::groups::messages_before(&state.pool, group_id, message_id, CONTEXT_BEFORE)
            .await
            .unwrap_or_default();

    let after = db::groups::messages_after(&state.pool, group_id, message_id, CONTEXT_AFTER)
        .await
        .unwrap_or_default();
    messages.extend(after);

    // Ensure the pivot itself is represented.
    if !messages.iter().any(|m| m.message_id == message_id) {
        messages.push(ContextMessage {
            user_id: None,
            username: None,
            message_id,
            text: String::new(),
            position: ContextPosition::Pivot,
        });
        messages.sort_by_key(|m| m.message_id);
    }

    ContextWindow {
        messages,
        forwarded,
        forward_origin,
    }
}

/// A display title for content that has none: first line, clipped.
fn derive_title(text: &str, content_type: &str) -> String {
    let first_line = text.lines().next().unwrap_or("").trim();
    let clipped: String = first_line.chars().take(60).collect();
    if clipped.is_empty() {
        match content_type {
            "voice" => "Voice note".to_string(),
            "image" => "Photo".to_string(),
            _ => "Note".to_string(),
        }
    } else if first_line.chars().count() > 60 {
        format!("{clipped}…")
    } else {
        clipped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_extracted_and_deduped() {
        let urls = extract_urls("see https://a.io/x and (https://b.io/y) plus https://a.io/x");
        assert_eq!(urls, vec!["https://a.io/x", "https://b.io/y"]);
    }

    #[test]
    fn titles_derived_sanely() {
        assert_eq!(derive_title("short note", "note"), "short note");
        assert_eq!(derive_title("", "voice"), "Voice note");
        assert!(derive_title(&"x".repeat(100), "note").ends_with('…'));
    }

    #[test]
    fn question_detection() {
        assert!(is_question("where was that ramen place"));
        assert!(is_question("that pasta recipe?"));
        assert!(!is_question("great pasta at Roscioli, get the carbonara"));
        assert!(!is_question(""));
    }

    #[test]
    fn deictic_queries_detected() {
        assert!(is_deictic_query("what do you think about this?"));
        assert!(is_deictic_query("thoughts on that link"));
        assert!(!is_deictic_query("what have we saved on robotics?"));
    }
}
