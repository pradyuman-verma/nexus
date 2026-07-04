//! YouTube transcript fetcher (Tier 1 variant for video links).
//!
//! Uses `yt-dlp -J` to get metadata + caption-track URLs in one call, then
//! fetches the English captions and parses the VTT cues into clean text. No
//! temp files and no ffmpeg required (we request the `vtt` track directly).
//! Falls back to the video description when no captions exist.

use crate::models::ExtractedContent;
use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::time::Duration;
use tokio::process::Command;

const YTDLP_TIMEOUT: Duration = Duration::from_secs(45);
const MAX_TRANSCRIPT_CHARS: usize = 40_000;

/// True if the URL points at YouTube (any of its host/path forms).
pub fn is_youtube(url: &str) -> bool {
    match url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
    {
        Some(host) => {
            let h = host.trim_start_matches("www.").to_lowercase();
            matches!(
                h.as_str(),
                "youtube.com" | "m.youtube.com" | "music.youtube.com" | "youtu.be"
            )
        }
        None => false,
    }
}

/// Fetch a YouTube video's transcript (or description fallback) via yt-dlp.
pub async fn fetch(url: &str, ytdlp_path: &str) -> Result<ExtractedContent> {
    let meta = run_ytdlp_json(url, ytdlp_path).await?;

    let title = meta
        .get("title")
        .and_then(Value::as_str)
        .map(str::to_string);
    let author = meta
        .get("uploader")
        .or_else(|| meta.get("channel"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let published = meta
        .get("upload_date")
        .and_then(Value::as_str)
        .map(str::to_string);
    let description = meta
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    // Prefer human-uploaded subtitles, then auto-captions.
    let caption_url = pick_caption_url(&meta, "subtitles")
        .or_else(|| pick_caption_url(&meta, "automatic_captions"));

    let transcript = match caption_url {
        Some(curl) => fetch_and_parse_vtt(&curl).await.unwrap_or_default(),
        None => String::new(),
    };

    let (text, available) = if transcript.chars().count() >= 200 {
        (truncate(&transcript, MAX_TRANSCRIPT_CHARS), true)
    } else if description.chars().count() >= 200 {
        // No usable captions — keep the description so the item is still useful.
        (description, false)
    } else {
        (description, false)
    };

    Ok(ExtractedContent {
        title,
        author,
        published,
        text,
        available,
    })
}

async fn run_ytdlp_json(url: &str, ytdlp_path: &str) -> Result<Value> {
    let fut = Command::new(ytdlp_path)
        .args([
            "-J",
            "--skip-download",
            "--no-warnings",
            "--no-playlist",
            url,
        ])
        .output();

    let out = tokio::time::timeout(YTDLP_TIMEOUT, fut)
        .await
        .map_err(|_| anyhow!("yt-dlp timed out"))?
        .with_context(|| format!("failed to run yt-dlp (is '{ytdlp_path}' installed?)"))?;

    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(anyhow!("yt-dlp failed: {}", err.trim()));
    }
    serde_json::from_slice(&out.stdout).context("parsing yt-dlp JSON")
}

/// Find an English caption track URL under `key` ("subtitles" / "automatic_captions"),
/// preferring the `vtt` format.
fn pick_caption_url(meta: &Value, key: &str) -> Option<String> {
    let tracks = meta.get(key)?.as_object()?;

    // Choose the best English-ish language key.
    let lang_key = tracks
        .keys()
        .find(|k| k.as_str() == "en")
        .or_else(|| tracks.keys().find(|k| k.starts_with("en")))?;

    let formats = tracks.get(lang_key)?.as_array()?;
    // Prefer vtt; else take the first track with a URL.
    formats
        .iter()
        .find(|f| f.get("ext").and_then(Value::as_str) == Some("vtt"))
        .or_else(|| formats.first())
        .and_then(|f| f.get("url").and_then(Value::as_str))
        .map(str::to_string)
}

async fn fetch_and_parse_vtt(url: &str) -> Result<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?;
    let body = client.get(url).send().await?.text().await?;
    Ok(vtt_to_text(&body))
}

/// Parse WebVTT into plain text: drop headers, timestamps and cue numbers,
/// strip inline `<...>` tags, and collapse consecutive duplicate lines (auto
/// captions repeat the rolling line).
fn vtt_to_text(vtt: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    for raw in vtt.lines() {
        let l = raw.trim();
        if l.is_empty()
            || l == "WEBVTT"
            || l.contains("-->")
            || l.starts_with("Kind:")
            || l.starts_with("Language:")
            || l.starts_with("NOTE")
            || l.chars().all(|c| c.is_ascii_digit())
        {
            continue;
        }
        let cleaned = strip_tags(l);
        let cleaned = cleaned.trim();
        if cleaned.is_empty() {
            continue;
        }
        if lines.last().map(|p| p == cleaned).unwrap_or(false) {
            continue; // collapse exact consecutive repeats
        }
        lines.push(cleaned.to_string());
    }
    lines.join(" ")
}

/// Remove `<...>` spans (timestamp/`<c>` cue tags) without a regex dependency.
fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth = 0u32;
    for c in s.chars() {
        match c {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_youtube_forms() {
        assert!(is_youtube("https://www.youtube.com/watch?v=abc123"));
        assert!(is_youtube("https://youtu.be/abc123"));
        assert!(is_youtube("https://m.youtube.com/watch?v=abc"));
        assert!(!is_youtube("https://example.com/watch?v=abc"));
        assert!(!is_youtube("https://vimeo.com/123"));
    }

    #[test]
    fn parses_vtt_and_dedups() {
        let vtt = "WEBVTT\n\n00:00:01.000 --> 00:00:03.000\nhello there\n\n\
                   00:00:03.000 --> 00:00:05.000\nhello there\n\n\
                   00:00:05.000 --> 00:00:07.000\n<c>general</c> kenobi\n";
        let text = vtt_to_text(vtt);
        assert_eq!(text, "hello there general kenobi");
    }
}
