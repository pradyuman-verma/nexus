//! Query handler: embed → pgvector search → Tier 5 synthesis → formatted reply.

use crate::bot::formatter;
use crate::db;
use crate::models::RetrievedItem;
use crate::state::AppState;
use anyhow::Result;

const SYSTEM_PROMPT: &str = "You are Nexus — a sharp research analyst and this \
group's collective memory. Using the group's shared sources, write an insightful, \
synthesized answer to the question. Think like a researcher, not a search engine:\n\
- Lead with the core thesis or through-line — not a list of restated points.\n\
- Connect ideas ACROSS sources; note where they reinforce, complicate, or contradict each other.\n\
- Surface what's non-obvious: implications, second-order effects, tensions, open questions.\n\
- Be concise and substantive. Avoid shallow bullet-point summaries of each source.\n\
Ground every factual claim in the sources and cite them as [n]; never invent facts, \
numbers, or sources. You MAY add interpretation and connect dots beyond the literal \
text, as long as the underlying facts trace to the sources. If the sources genuinely \
can't support an answer, set \"answerable\" to false.\n\
Write the answer as plain text — no markdown (no **bold**, #headings, or backticks). \
Return JSON only, no prose outside the JSON, in exactly this shape: \
{\"answerable\": true|false, \
\"answer\": \"your analysis, citing sources as [1], [2]; empty string if not answerable\", \
\"sources\": [the source numbers you actually drew on; empty if not answerable]}";

const TOP_K: i64 = 8;
const MIN_SIMILARITY: f32 = 0.4;
const NO_RESULTS: &str =
    "Nothing in our shared history covers that yet. Keep sharing and I'll get smarter.";

/// Handle a `/ask` / @mention query. Returns the HTML reply to send.
pub async fn handle(
    state: &AppState,
    group_id: i64,
    user_id: i64,
    query_text: &str,
) -> Result<String> {
    let query_text = query_text.trim();
    if query_text.is_empty() {
        return Ok(
            "Ask me something — e.g. <code>/ask what have we seen on robotics funding?</code>"
                .to_string(),
        );
    }

    // Tier 3: embed the query.
    let qvec = state.embedder.embed(query_text).await?;

    // Querying is the strongest interest signal (weight 3.0).
    let _ = db::profiles::apply_weighted_update(
        &state.pool,
        user_id,
        group_id,
        &qvec,
        3.0,
        state.config.max_vector_weight,
        state.config.default_relevance_threshold,
        &[],
        true,
    )
    .await;

    // pgvector search.
    let items = db::items::search(&state.pool, group_id, &qvec, TOP_K, MIN_SIMILARITY).await?;
    tracing::info!(
        group_id,
        user_id,
        retrieved = items.len(),
        query = %snippet(query_text, 80),
        "ask"
    );
    if items.is_empty() {
        return Ok(NO_RESULTS.to_string());
    }

    // Tier 5: synthesize.
    let context = build_context(&items);
    let user_msg = format!("Sources:\n{context}\n\nQuestion: {query_text}");
    let raw = state.chat.synthesize(SYSTEM_PROMPT, &user_msg).await?;

    let resolved = resolve_answer(&raw, items.len());
    match resolved {
        Some((answer, cited)) => {
            tracing::info!(group_id, answered = true, cited = cited.len(), "ask answered");
            Ok(formatter::query_answer(&answer, &items, &cited, group_id))
        }
        None => {
            tracing::info!(group_id, answered = false, "ask answered");
            Ok(NO_RESULTS.to_string())
        }
    }
}

/// Turn the model's raw response into `(answer_text, cited_indices)`, or None
/// when there's no usable answer. Robust to schema drift: accepts our strict
/// `{answerable, answer, sources}` shape, alternative keys (`text`/`response`/
/// `summary`), and plain prose — and NEVER surfaces raw JSON to the user.
fn resolve_answer(raw: &str, n: usize) -> Option<(String, Vec<usize>)> {
    let cleaned = strip_fences(raw);

    // Try to parse a JSON object out of the response.
    let json: Option<serde_json::Value> = serde_json::from_str(cleaned).ok().or_else(|| {
        let (s, e) = (cleaned.find('{'), cleaned.rfind('}'));
        match (s, e) {
            (Some(s), Some(e)) if e > s => serde_json::from_str(&cleaned[s..=e]).ok(),
            _ => None,
        }
    });

    if let Some(v) = &json {
        // Explicit "not answerable" → no answer.
        if v.get("answerable").and_then(serde_json::Value::as_bool) == Some(false) {
            return None;
        }
        // Pull the answer text from whichever key the model used.
        let answer = ["answer", "text", "response", "summary"]
            .iter()
            .find_map(|k| v.get(*k).and_then(serde_json::Value::as_str))
            .map(str::trim)
            .filter(|s| !s.is_empty());

        if let Some(answer) = answer {
            // Prefer an explicit, valid sources array; else infer from [n] markers.
            let explicit: Vec<usize> = v
                .get("sources")
                .and_then(serde_json::Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(serde_json::Value::as_u64)
                        .map(|x| x as usize)
                        .collect()
                })
                .unwrap_or_default();
            let cited = sanitize_cited(&explicit, n);
            let cited = if cited.is_empty() {
                cited_in_text(answer, n)
            } else {
                cited
            };
            return Some((answer.to_string(), cited));
        }
        // It was JSON but we couldn't find an answer field — don't leak braces.
        return None;
    }

    // Not JSON at all → treat as plain prose, infer citations from the text.
    let prose = cleaned.trim();
    if prose.is_empty() || prose.starts_with('{') {
        return None;
    }
    Some((prose.to_string(), cited_in_text(prose, n)))
}

fn strip_fences(raw: &str) -> &str {
    raw.trim()
        .strip_prefix("```json")
        .or_else(|| raw.trim().strip_prefix("```"))
        .unwrap_or(raw.trim())
        .trim_end_matches("```")
        .trim()
}

/// Keep only in-range, deduped, ascending source numbers (1-based).
fn sanitize_cited(sources: &[usize], n: usize) -> Vec<usize> {
    let mut v: Vec<usize> = sources.iter().copied().filter(|&s| s >= 1 && s <= n).collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// Scan answer text for `[k]` citation markers and return the valid ones.
fn cited_in_text(text: &str, n: usize) -> Vec<usize> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        rest = &rest[open + 1..];
        if let Some(close) = rest.find(']') {
            if let Ok(k) = rest[..close].trim().parse::<usize>() {
                if k >= 1 && k <= n {
                    out.push(k);
                }
            }
            rest = &rest[close + 1..];
        } else {
            break;
        }
    }
    sanitize_cited(&out, n)
}

/// How much actual article text to give the model per source. Distributed across
/// the retrieved items, this keeps the prompt rich but bounded.
const CONTENT_BUDGET_CHARS: usize = 2200;

fn build_context(items: &[RetrievedItem]) -> String {
    let mut out = String::new();
    for (i, item) in items.iter().enumerate() {
        let n = i + 1;
        let label = formatter::item_label(item);
        let summary = item.summary.as_deref().unwrap_or("(no summary)");
        let tags = if item.tags.is_empty() {
            String::new()
        } else {
            format!("\n   tags: {}", item.tags.join(", "))
        };
        let who = item
            .shared_by_username
            .as_deref()
            .map(|u| format!("@{u}"))
            .unwrap_or_else(|| "someone".to_string());
        let date = item.shared_at.format("%Y-%m-%d");
        let ctx = item
            .context_window
            .as_ref()
            .map(|c| c.as_text())
            .filter(|t| !t.is_empty())
            .map(|t| format!("\n   conversation when shared: {}", snippet(&t, 280)))
            .unwrap_or_default();
        // The actual article/transcript text — what lets the model reason instead
        // of just restating the summary.
        let content = item
            .raw_content
            .as_deref()
            .map(|c| c.trim())
            .filter(|c| !c.is_empty())
            .map(|c| format!("\n   content: {}", snippet(c, CONTENT_BUDGET_CHARS)))
            .unwrap_or_default();

        out.push_str(&format!(
            "[{n}] {label} ({url})\n   shared by {who} on {date}{tags}\n   summary: {summary}{content}{ctx}\n\n",
            url = item.url,
        ));
    }
    out
}

fn snippet(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.replace('\n', " ")
    } else {
        let t: String = s.chars().take(max).collect();
        format!("{}…", t.replace('\n', " "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_schema_with_citations() {
        let (ans, cited) = resolve_answer(r#"{"answerable":true,"answer":"hi [2]","sources":[2]}"#, 3).unwrap();
        assert_eq!(ans, "hi [2]");
        assert_eq!(cited, vec![2]);
    }

    #[test]
    fn wrong_shape_does_not_leak_json() {
        // The bug from the field: model returned {"text": ...}. Must extract the
        // text, never surface raw braces.
        let (ans, cited) = resolve_answer(r#"{"text":"some answer"}"#, 5).unwrap();
        assert_eq!(ans, "some answer");
        assert!(cited.is_empty());
    }

    #[test]
    fn explicit_not_answerable_is_none() {
        assert!(resolve_answer(r#"{"answerable":false,"answer":"","sources":[]}"#, 3).is_none());
    }

    #[test]
    fn plain_prose_infers_citations() {
        let (ans, cited) = resolve_answer("Plain answer drawing on [1] and [3].", 3).unwrap();
        assert_eq!(ans, "Plain answer drawing on [1] and [3].");
        assert_eq!(cited, vec![1, 3]);
    }

    #[test]
    fn citations_inferred_when_array_missing() {
        let (_, cited) = resolve_answer(r#"{"answer":"uses [1] and [2]"}"#, 4).unwrap();
        assert_eq!(cited, vec![1, 2]);
    }

    #[test]
    fn fenced_json_is_parsed() {
        let (ans, _) = resolve_answer("```json\n{\"answerable\":true,\"answer\":\"x\",\"sources\":[]}\n```", 2).unwrap();
        assert_eq!(ans, "x");
    }

    #[test]
    fn out_of_range_citations_dropped() {
        let cited = cited_in_text("refs [1] [9] [2]", 3);
        assert_eq!(cited, vec![1, 2]);
    }
}
