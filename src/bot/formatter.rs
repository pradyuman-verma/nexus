//! Telegram message formatting (HTML parse mode) and deep-link construction.

use crate::models::RetrievedItem;
use teloxide::types::LinkPreviewOptions;

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
        let who = item
            .shared_by_username
            .as_deref()
            .map(|u| format!("@{u}"))
            .unwrap_or_else(|| "someone".to_string());
        let date = item.shared_at.format("%Y-%m-%d");
        let link = item
            .message_id
            .and_then(|m| message_link(chat_id, m))
            .unwrap_or_else(|| item.url.clone());
        out.push_str(&format!(
            "\n{n}. <a href=\"{}\">{label}</a> — shared by {who} on {date}",
            esc(&link)
        ));
    }
    out
}
