//! Query handler: embed → pgvector search → Tier 5 synthesis → formatted reply.

use crate::bot::formatter;
use crate::db;
use crate::models::RetrievedItem;
use crate::state::AppState;
use anyhow::Result;
use serde::Deserialize;

const SYSTEM_PROMPT: &str = "You are Nexus, this group's collective memory. \
Answer the user's question using ONLY the numbered sources provided. \
Set \"answerable\" to false when the sources are off-topic or simply don't contain \
the answer — do NOT stretch a loosely-related source into an answer. \
Never invent facts that aren't in the sources. \
Return JSON only, no prose, in exactly this shape: \
{\"answerable\": true|false, \
\"answer\": \"1-3 short paragraphs citing sources as [1], [2]; empty string if not answerable\", \
\"sources\": [the source numbers you actually used; empty if not answerable]}";

const TOP_K: i64 = 10;
const MIN_SIMILARITY: f32 = 0.25;
const NO_RESULTS: &str =
    "Nothing in our shared history covers that yet. Keep sharing and I'll get smarter.";

#[derive(Deserialize)]
struct Tier5Answer {
    #[serde(default)]
    answerable: bool,
    #[serde(default)]
    answer: String,
    #[serde(default)]
    sources: Vec<usize>,
}

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
    if items.is_empty() {
        return Ok(NO_RESULTS.to_string());
    }

    // Tier 5: synthesize.
    let context = build_context(&items);
    let user_msg = format!("Sources:\n{context}\n\nQuestion: {query_text}");
    let raw = state.chat.synthesize(SYSTEM_PROMPT, &user_msg).await?;

    match parse_answer(&raw) {
        // Model answered: show the answer with only the sources it actually cited.
        Some(a) if a.answerable && !a.answer.trim().is_empty() => {
            let cited = sanitize_cited(&a.sources, items.len());
            Ok(formatter::query_answer(&a.answer, &items, &cited, group_id))
        }
        // Model said the corpus doesn't answer it — no source dump.
        Some(_) => Ok(NO_RESULTS.to_string()),
        // Couldn't parse JSON — fall back to the raw text with all sources.
        None => {
            let all: Vec<usize> = (1..=items.len()).collect();
            Ok(formatter::query_answer(&raw, &items, &all, group_id))
        }
    }
}

/// Keep only in-range, deduped, ascending source numbers (1-based).
fn sanitize_cited(sources: &[usize], n: usize) -> Vec<usize> {
    let mut v: Vec<usize> = sources
        .iter()
        .copied()
        .filter(|&s| s >= 1 && s <= n)
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// Tolerantly parse the Tier 5 JSON (handles ```json fences / surrounding prose).
fn parse_answer(raw: &str) -> Option<Tier5Answer> {
    let cleaned = raw
        .trim()
        .strip_prefix("```json")
        .or_else(|| raw.trim().strip_prefix("```"))
        .unwrap_or(raw.trim())
        .trim_end_matches("```")
        .trim();
    if let Ok(a) = serde_json::from_str::<Tier5Answer>(cleaned) {
        return Some(a);
    }
    let (s, e) = (cleaned.find('{'), cleaned.rfind('}'));
    if let (Some(s), Some(e)) = (s, e) {
        if e > s {
            return serde_json::from_str::<Tier5Answer>(&cleaned[s..=e]).ok();
        }
    }
    None
}

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

        out.push_str(&format!(
            "[{n}] {label} ({url})\n   summary: {summary}{tags}\n   shared by {who} on {date}{ctx}\n\n",
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
