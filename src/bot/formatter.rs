//! Telegram message formatting (HTML parse mode) and deep-link construction.

use crate::models::RetrievedItem;
use chrono::{DateTime, Utc};
use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup, LinkPreviewOptions};
use uuid::Uuid;

/// Link-preview options that fully disable the preview bubble.
pub fn no_preview() -> LinkPreviewOptions {
    LinkPreviewOptions {
        is_disabled: true,
        url: None,
        prefer_small_media: false,
        prefer_large_media: false,
        show_above_text: false,
    }
}

/// Escape text for Telegram HTML parse mode.
pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Build a `t.me/c/<id>/<msg>` deep link back to the original group message.
/// Returns None for non-supergroup chats (DMs, basic groups) which have no link.
pub fn message_link(chat_id: i64, message_id: i64) -> Option<String> {
    // Supergroup/channel ids look like -100XXXXXXXXXX; the link uses the XXXX part.
    let s = chat_id.to_string();
    let internal = s.strip_prefix("-100")?;
    Some(format!("https://t.me/c/{internal}/{message_id}"))
}

/// Best display label for an item: title, else domain, else the raw URL.
pub fn item_label(item: &RetrievedItem) -> String {
    if let Some(t) = &item.title {
        if !t.trim().is_empty() {
            return t.clone();
        }
    }
    domain_of(&item.url).unwrap_or_else(|| item.url.clone())
}

pub fn domain_of(url: &str) -> Option<String> {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.trim_start_matches("www.").to_string()))
}

/// Human channel name for provenance, e.g. "Telegram" / "WhatsApp".
pub fn channel_label(source_channel: Option<&str>) -> &'static str {
    match source_channel {
        Some("telegram") => "Telegram",
        Some("whatsapp") => "WhatsApp",
        Some(_) => "Other",
        None => "Unknown",
    }
}

/// Short date for source lines, e.g. "Jun 4".
pub fn short_date(dt: DateTime<Utc>) -> String {
    dt.format("%b %-d").to_string()
}

/// Provenance suffix: "Telegram · Jun 4".
pub fn source_provenance(item: &RetrievedItem) -> String {
    format!(
        "{} · {}",
        channel_label(item.source_channel.as_deref()),
        short_date(item.shared_at)
    )
}

/// The personal relevance-interrupt notification (HTML).
pub fn notification(
    sharer_name: &str,
    title: &str,
    url: &str,
    excerpt: &str,
    score: f32,
    matching_tag: Option<&str>,
    link: Option<&str>,
) -> String {
    let pct = (score * 100.0).round() as i32;
    let why = matching_tag
        .map(|t| format!("\nWhy: matched your interest in <b>{}</b>", esc(t)))
        .unwrap_or_default();
    let target = link.unwrap_or(url);

    format!(
        "📌 <b>{sharer}</b> shared something for you.\n\n\
         <b>{title}</b>\n\n\
         <i>\"{excerpt}\"</i>{why}\n\
         Score: {pct}% match\n\n\
         → <a href=\"{target}\">open original</a>",
        sharer = esc(sharer_name),
        title = esc(title),
        excerpt = esc(excerpt.trim().trim_matches('"')),
        target = esc(target),
    )
}

/// Format a query answer, listing only the sources the model actually cited
/// (`cited` holds 1-based indices into `items`). With no cited sources, the
/// answer stands alone — no "Sources" dump.
pub fn query_answer(
    answer: &str,
    items: &[RetrievedItem],
    cited: &[usize],
    chat_id: i64,
) -> String {
    let mut out = esc(answer.trim());
    if cited.is_empty() {
        return out;
    }
    out.push_str("\n\n<b>Sources:</b>");
    for &n in cited {
        let Some(item) = items.get(n - 1) else { continue };
        let label = esc(&item_label(item));
        let provenance = esc(&source_provenance(item));
        let link = item
            .message_id
            .and_then(|m| message_link(chat_id, m))
            .unwrap_or_else(|| item.url.clone());
        out.push_str(&format!(
            "\n[{n}] <a href=\"{}\">{label}</a> ({provenance})",
            esc(&link)
        ));
    }
    out
}

/// 👍/👎 feedback row for /ask replies. 👎 encodes the primary cited item id.
pub fn ask_feedback_keyboard(primary_item_id: Option<Uuid>) -> InlineKeyboardMarkup {
    let mut row = vec![InlineKeyboardButton::callback("👍", "au")];
    if let Some(id) = primary_item_id {
        row.push(InlineKeyboardButton::callback("👎", format!("ad:{id}")));
    }
    InlineKeyboardMarkup::new(vec![row])
}

/// "Not relevant" dismiss button for relevance DMs.
pub fn notification_keyboard(item_id: Uuid) -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![vec![InlineKeyboardButton::callback(
        "Not relevant",
        format!("nd:{item_id}"),
    )]])
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn sample_item(channel: &str, title: &str) -> RetrievedItem {
        RetrievedItem {
            id: Uuid::new_v4(),
            url: "https://example.com".into(),
            title: Some(title.into()),
            summary: None,
            raw_content: None,
            tags: vec![],
            category: None,
            context_window: None,
            shared_by: None,
            shared_by_username: None,
            message_id: None,
            shared_at: Utc::now(),
            similarity: 0.0,
            source_channel: Some(channel.into()),
            content_type: Some("article".into()),
        }
    }

    #[test]
    fn source_provenance_formats_channel_and_date() {
        let item = sample_item("whatsapp", "Voice note");
        let p = source_provenance(&item);
        assert!(p.starts_with("WhatsApp · "));
    }

    #[test]
    fn query_answer_shows_provenance() {
        let items = vec![sample_item("telegram", "Robotics fund")];
        let out = query_answer("Answer [1].", &items, &[1], -100123);
        assert!(out.contains("Robotics fund"));
        assert!(out.contains("Telegram ·"));
        assert!(out.contains("[1]"));
    }
}
