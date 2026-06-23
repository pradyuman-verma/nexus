//! Tier 1: fetch a URL and extract clean article text.
//!
//! A lightweight, dependency-light Readability-style extractor: it pulls
//! og:/title/author/published metadata, then collects text from the main
//! content container while dropping nav/header/footer/aside/script/style.
//! Falls back to `html2text` over the whole document when that yields too little.

use crate::models::ExtractedContent;
use anyhow::{anyhow, Result};
use scraper::{Html, Selector};
use std::time::Duration;

const USER_AGENT: &str =
    "Mozilla/5.0 (compatible; NexusBot/0.1; +https://github.com/nexus-bot)";
const MIN_BODY_CHARS: usize = 200;

/// Fetch + extract. On HTTP/transport failure returns an `Err`; on a soft failure
/// (paywall page, JS-only SPA) returns `Ok` with `available = false` and whatever
/// metadata we could scrape.
pub async fn fetch(url: &str) -> Result<ExtractedContent> {
    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()?;

    let resp = client.get(url).send().await?;
    let status = resp.status();
    if !status.is_success() {
        return Err(anyhow!("fetch {url} returned {status}"));
    }

    let html = resp.text().await?;
    Ok(extract(&html))
}

/// Pure extraction over an HTML string (also used in tests).
pub fn extract(html: &str) -> ExtractedContent {
    let doc = Html::parse_document(html);

    let title = meta(&doc, "og:title")
        .or_else(|| meta_name(&doc, "twitter:title"))
        .or_else(|| text_of(&doc, "title"));
    let author = meta_name(&doc, "author").or_else(|| meta(&doc, "article:author"));
    let published = meta(&doc, "article:published_time")
        .or_else(|| meta_name(&doc, "date"));
    let description = meta(&doc, "og:description").or_else(|| meta_name(&doc, "description"));

    let body = extract_body(&doc);

    let text = if body.chars().count() >= MIN_BODY_CHARS {
        body
    } else {
        // Fallback 1: html2text over the whole doc.
        let rendered = html2text::from_read(html.as_bytes(), 100)
            .trim()
            .to_string();
        if rendered.chars().count() >= MIN_BODY_CHARS {
            rendered
        } else {
            // Fallback 2: at least keep the social description.
            description.clone().unwrap_or_default()
        }
    };

    let available = text.chars().count() >= MIN_BODY_CHARS;

    ExtractedContent {
        title,
        author,
        published,
        text,
        available,
    }
}

fn extract_body(doc: &Html) -> String {
    // Prefer a semantic main-content container.
    for sel in ["article", "main", "[role=main]"] {
        if let Ok(selector) = Selector::parse(sel) {
            if let Some(el) = doc.select(&selector).next() {
                let frag = Html::parse_fragment(&el.html());
                let text = collect_text(&frag);
                if text.chars().count() >= MIN_BODY_CHARS {
                    return text;
                }
            }
        }
    }
    // Fall back to the whole body, minus boilerplate handled by collecting only
    // content elements.
    collect_text(doc)
}

/// Collect text from content-bearing elements only (paragraphs, headings, list
/// items, blockquotes), which naturally skips nav/sidebar/script/style chrome.
fn collect_text(doc: &Html) -> String {
    let selector = Selector::parse("p, h1, h2, h3, h4, li, blockquote").unwrap();
    let mut parts = Vec::new();
    for el in doc.select(&selector) {
        let t: String = el.text().collect::<Vec<_>>().join(" ");
        let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
        if t.chars().count() > 1 {
            parts.push(t);
        }
    }
    parts.join("\n")
}

fn meta(doc: &Html, property: &str) -> Option<String> {
    let sel = Selector::parse(&format!(r#"meta[property="{property}"]"#)).ok()?;
    doc.select(&sel)
        .next()
        .and_then(|e| e.value().attr("content"))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn meta_name(doc: &Html, name: &str) -> Option<String> {
    let sel = Selector::parse(&format!(r#"meta[name="{name}"]"#)).ok()?;
    doc.select(&sel)
        .next()
        .and_then(|e| e.value().attr("content"))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn text_of(doc: &Html, tag: &str) -> Option<String> {
    let sel = Selector::parse(tag).ok()?;
    doc.select(&sel)
        .next()
        .map(|e| e.text().collect::<String>().trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Truncate to roughly `max_tokens` (~4 chars/token) before an LLM call.
pub fn truncate_for_llm(text: &str, max_tokens: usize) -> String {
    let max_chars = max_tokens * 4;
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        text.chars().take(max_chars).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_title_and_body() {
        let html = r#"
            <html><head>
              <meta property="og:title" content="Hello World">
            </head><body>
              <nav>menu menu menu</nav>
              <article>
                <p>This is the first substantial paragraph of the article body that
                   should be picked up by the extractor without any of the chrome.</p>
                <p>And here is a second paragraph adding more real content so the
                   total length comfortably exceeds the minimum threshold value.</p>
              </article>
            </body></html>"#;
        let c = extract(html);
        assert_eq!(c.title.as_deref(), Some("Hello World"));
        assert!(c.available);
        assert!(c.text.contains("first substantial paragraph"));
    }
}
