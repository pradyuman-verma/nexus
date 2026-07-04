//! Convert Nexus's Telegram-flavored HTML replies to WhatsApp plain text.
//! WhatsApp has no HTML: tags are stripped, links inlined, entities decoded.

/// Strip HTML tags, inlining <a href> targets after their anchor text.
pub fn html_to_plain(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut chars = html.chars().peekable();
    let mut pending_href: Option<String> = None;

    while let Some(c) = chars.next() {
        if c != '<' {
            out.push(c);
            continue;
        }
        // Collect the tag body up to '>'.
        let mut tag = String::new();
        for t in chars.by_ref() {
            if t == '>' {
                break;
            }
            tag.push(t);
        }
        let lower = tag.to_lowercase();
        if lower.starts_with("a ") || lower == "a" {
            pending_href = extract_href(&tag);
        } else if lower == "/a" {
            if let Some(href) = pending_href.take() {
                // Inline only real web links — internal pseudo-URLs
                // (note://, voice://, image://) are noise to a human.
                if href.starts_with("http") && !out.trim_end().ends_with(href.as_str()) {
                    out.push_str(&format!(" ({href})"));
                }
            }
        } else if lower == "br" || lower == "br/" || lower == "br /" {
            out.push('\n');
        }
        // Every other tag (<b>, <i>, <code>, …) is dropped.
    }

    decode_entities(&out).trim().to_string()
}

fn extract_href(tag: &str) -> Option<String> {
    let idx = tag.to_lowercase().find("href=")?;
    let rest = &tag[idx + 5..];
    let quote = rest.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let rest = &rest[1..];
    let end = rest.find(quote)?;
    Some(decode_entities(&rest[..end]))
}

fn decode_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_tags_and_inlines_links() {
        let html = r#"<b>Answer</b>: see [1]<br><a href="https://x.io/a?b=1&amp;c=2">Great post</a>"#;
        assert_eq!(
            html_to_plain(html),
            "Answer: see [1]\nGreat post (https://x.io/a?b=1&c=2)"
        );
    }

    #[test]
    fn plain_text_untouched() {
        assert_eq!(html_to_plain("no tags & no entities"), "no tags & no entities");
    }

    #[test]
    fn skips_href_when_anchor_is_the_url() {
        let html = r#"<a href="https://x.io/a">https://x.io/a</a>"#;
        assert_eq!(html_to_plain(html), "https://x.io/a");
    }

    #[test]
    fn hides_internal_pseudo_urls() {
        let html = r#"<a href="note://900/17831">my carbonara note</a>"#;
        assert_eq!(html_to_plain(html), "my carbonara note");
    }
}
