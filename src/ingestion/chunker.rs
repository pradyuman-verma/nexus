//! Split document text into overlapping passages for embedding.
//!
//! Windows are sized in characters (kept under the embedding model's context —
//! mxbai-embed-large is 512 tokens ≈ ~2000 chars) and cut on whitespace so words
//! aren't split. Consecutive chunks overlap so a sentence spanning a boundary is
//! still fully present in at least one chunk.

/// Default passage size (chars) — comfortably under a 512-token embed context.
pub const TARGET_CHARS: usize = 1500;
/// Overlap between consecutive passages (chars).
pub const OVERLAP_CHARS: usize = 150;
/// Safety cap so a multi-hour transcript can't explode into hundreds of chunks.
pub const MAX_CHUNKS: usize = 50;

pub fn chunk_text(text: &str) -> Vec<String> {
    chunk_with(text, TARGET_CHARS, OVERLAP_CHARS)
}

pub fn chunk_with(text: &str, target: usize, overlap: usize) -> Vec<String> {
    let text = text.trim();
    if text.is_empty() {
        return Vec::new();
    }
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= target {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut start = 0;
    while start < chars.len() && chunks.len() < MAX_CHUNKS {
        let mut end = (start + target).min(chars.len());
        // Back up to the last whitespace so we don't cut mid-word — but only if
        // that still leaves a reasonably full chunk.
        if end < chars.len() {
            let mut e = end;
            while e > start && !chars[e - 1].is_whitespace() {
                e -= 1;
            }
            if e > start + target / 2 {
                end = e;
            }
        }

        let chunk: String = chars[start..end].iter().collect();
        let chunk = chunk.trim().to_string();
        if !chunk.is_empty() {
            chunks.push(chunk);
        }

        if end >= chars.len() {
            break;
        }
        // Advance, keeping `overlap` chars of context; always make progress.
        let next = end.saturating_sub(overlap);
        start = if next > start { next } else { end };
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_is_one_chunk() {
        assert_eq!(chunk_text("hello world"), vec!["hello world"]);
    }

    #[test]
    fn long_text_splits_with_overlap_and_progress() {
        let text = "word ".repeat(1000); // 5000 chars
        let chunks = chunk_with(&text, 1000, 100);
        assert!(chunks.len() > 1);
        // Every chunk is within bounds and non-empty.
        assert!(chunks.iter().all(|c| !c.is_empty() && c.chars().count() <= 1000));
    }

    #[test]
    fn respects_max_chunks() {
        let text = "a ".repeat(100_000);
        assert!(chunk_with(&text, 500, 50).len() <= MAX_CHUNKS);
    }
}
