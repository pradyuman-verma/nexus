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
can't support an answer, set \"answerable\" to false and put 2-3 search queries that \
WOULD locate the missing information into \"follow_up\".\n\
Write the answer as plain text — no markdown (no **bold**, #headings, or backticks). \
Return JSON only, no prose outside the JSON, in exactly this shape: \
{\"answerable\": true|false, \
\"answer\": \"your analysis, citing sources as [1], [2]; empty string if not answerable\", \
\"sources\": [the source numbers you actually drew on; empty if not answerable], \
\"follow_up\": [\"search queries to find missing info; empty when answerable\"]}";

/// How many passages to pull before grouping them back under their items.
const CHUNK_K: i64 = 16;
/// Cap on distinct source items shown to the model / cited.
const MAX_SOURCES: usize = 6;
/// Cap on passages quoted per source.
const MAX_PASSAGES_PER_ITEM: usize = 3;
/// Vector floor for passage retrieval. Passage embeddings score lower against a
/// short query than the old item-level (summary) embeddings did, so this is
/// intentionally lenient — precision is recovered by the keyword half of hybrid
/// search, the graph filter, and the model's own "answerable" gate.
const MIN_SIMILARITY: f32 = 0.25;
const NO_RESULTS: &str =
    "Nothing in our shared history covers that yet. Keep sharing and I'll get smarter.";

/// A source item with the passages that matched the query.
struct Source {
    item: RetrievedItem,
    passages: Vec<String>,
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

    // Tier 3: embed the query (also the interest signal below).
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

    // Multi-query / HyDE: expand the question into extra probes + a hypothetical
    // answer, embed them all, and retrieve over the union (failure-tolerant —
    // falls back to just the original query).
    let expansion = state.chat.expand_query(query_text).await.unwrap_or_default();
    let mut embeds = vec![qvec.clone()];
    let extra_probes: Vec<String> = expansion
        .probes
        .into_iter()
        .chain(std::iter::once(expansion.hyde))
        .filter(|p| !p.trim().is_empty())
        .collect();
    for probe in &extra_probes {
        if let Ok(e) = state.embedder.embed(probe).await {
            embeds.push(e);
        }
    }
    tracing::info!(group_id, user_id, probes = extra_probes.len(), query = %snippet(query_text, 80), "ask expand");

    // Retrieve (hybrid + graph), synthesize, and self-correct once if the model
    // says the corpus can't answer and offers follow-up queries.
    let mut keyword_query = query_text.to_string();
    for attempt in 0..2u8 {
        let hits = hybrid_retrieve(state, group_id, &keyword_query, &embeds).await;
        let mut sources = group_hits(hits);
        let graph_added = expand_with_graph(state, group_id, query_text, &qvec, &mut sources).await;

        if sources.is_empty() {
            return Ok(NO_RESULTS.to_string());
        }
        tracing::info!(group_id, attempt, sources = sources.len(), graph_added, "ask context");

        let items: Vec<RetrievedItem> = sources.iter().map(|s| s.item.clone()).collect();
        let context = build_context(&sources);
        let user_msg = format!("Sources:\n{context}\n\nQuestion: {query_text}");
        let raw = state.chat.synthesize(SYSTEM_PROMPT, &user_msg).await?;

        match resolve_outcome(&raw, items.len()) {
            AnswerOutcome::Answered { answer, cited } => {
                tracing::info!(group_id, attempt, answered = true, cited = cited.len(), "ask answered");
                return Ok(formatter::query_answer(&answer, &items, &cited, group_id));
            }
            AnswerOutcome::NeedMore { follow_up } => {
                // Out of retries, or no leads to chase → give up gracefully.
                if attempt >= 1 || follow_up.is_empty() {
                    tracing::info!(group_id, attempt, answered = false, "ask answered");
                    return Ok(NO_RESULTS.to_string());
                }
                // Second-pass retrieval targeting the gap the model flagged.
                tracing::info!(group_id, follow_ups = follow_up.len(), "ask self-correcting");
                for q in &follow_up {
                    if let Ok(e) = state.embedder.embed(q).await {
                        embeds.push(e);
                    }
                }
                keyword_query = format!("{query_text} {}", follow_up.join(" "));
            }
        }
    }

    Ok(NO_RESULTS.to_string())
}

/// Hybrid retrieval over multiple query embeddings: vector search per embedding
/// + one keyword search, all fused with RRF.
async fn hybrid_retrieve(
    state: &AppState,
    group_id: i64,
    keyword_query: &str,
    embeds: &[Vec<f32>],
) -> Vec<crate::db::chunks::ChunkHit> {
    let mut lists = Vec::new();
    for e in embeds {
        if let Ok(h) = db::chunks::search(&state.pool, group_id, e, CHUNK_K, MIN_SIMILARITY).await {
            lists.push(h);
        }
    }
    match db::chunks::keyword_search(&state.pool, group_id, keyword_query, CHUNK_K).await {
        Ok(k) => lists.push(k),
        Err(e) => tracing::debug!(error = %e, "keyword search failed"),
    }
    rrf_fuse(lists, CHUNK_K as usize)
}

/// Outcome of resolving a Tier 5 response.
enum AnswerOutcome {
    Answered { answer: String, cited: Vec<usize> },
    NeedMore { follow_up: Vec<String> },
}

/// Resolve a Tier 5 response into an outcome. Robust to schema drift: accepts our
/// strict `{answerable, answer, sources, follow_up}` shape, alternative answer keys
/// (`text`/`response`/`summary`), and plain prose — and NEVER surfaces raw JSON.
fn resolve_outcome(raw: &str, n: usize) -> AnswerOutcome {
    let cleaned = strip_fences(raw);

    let json: Option<serde_json::Value> = serde_json::from_str(cleaned).ok().or_else(|| {
        let (s, e) = (cleaned.find('{'), cleaned.rfind('}'));
        match (s, e) {
            (Some(s), Some(e)) if e > s => serde_json::from_str(&cleaned[s..=e]).ok(),
            _ => None,
        }
    });

    if let Some(v) = &json {
        let follow_up = string_array(v, "follow_up");

        // Explicit "not answerable" → ask for more, with the model's leads.
        if v.get("answerable").and_then(serde_json::Value::as_bool) == Some(false) {
            return AnswerOutcome::NeedMore { follow_up };
        }
        let answer = ["answer", "text", "response", "summary"]
            .iter()
            .find_map(|k| v.get(*k).and_then(serde_json::Value::as_str))
            .map(str::trim)
            .filter(|s| !s.is_empty());

        if let Some(answer) = answer {
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
            return AnswerOutcome::Answered {
                answer: answer.to_string(),
                cited,
            };
        }
        // JSON but no answer field — don't leak braces; treat as "need more".
        return AnswerOutcome::NeedMore { follow_up };
    }

    // Not JSON → plain prose answer, infer citations from the text.
    let prose = cleaned.trim();
    if prose.is_empty() || prose.starts_with('{') {
        return AnswerOutcome::NeedMore {
            follow_up: Vec::new(),
        };
    }
    AnswerOutcome::Answered {
        answer: prose.to_string(),
        cited: cited_in_text(prose, n),
    }
}

/// Extract a JSON string array field, trimming/dropping empties.
fn string_array(v: &serde_json::Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(serde_json::Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(serde_json::Value::as_str)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
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

/// Expand `sources` with items the graph connects to entities named in the query.
/// Returns how many new sources were added.
async fn expand_with_graph(
    state: &AppState,
    group_id: i64,
    query_text: &str,
    qvec: &[f32],
    sources: &mut Vec<Source>,
) -> usize {
    if sources.len() >= MAX_SOURCES {
        return 0;
    }
    let seeds = db::graph::entities_in_query(&state.pool, group_id, query_text, 8)
        .await
        .unwrap_or_default();
    if seeds.is_empty() {
        return 0;
    }

    // Seeds + one hop of related entities.
    let mut entities = seeds.clone();
    if let Ok(related) = db::graph::related_entities(&state.pool, group_id, &seeds, 12).await {
        entities.extend(related);
    }

    let item_ids = db::graph::items_mentioning(&state.pool, group_id, &entities, 25)
        .await
        .unwrap_or_default();

    let present: std::collections::HashSet<uuid::Uuid> =
        sources.iter().map(|s| s.item.id).collect();
    let new_ids: Vec<uuid::Uuid> = item_ids.into_iter().filter(|id| !present.contains(id)).collect();
    if new_ids.is_empty() {
        return 0;
    }

    let extra = db::chunks::search_within_items(
        &state.pool,
        group_id,
        qvec,
        &new_ids,
        (MAX_SOURCES * MAX_PASSAGES_PER_ITEM) as i64,
    )
    .await
    .unwrap_or_default();

    let mut added = 0;
    for src in group_hits(extra) {
        if sources.len() >= MAX_SOURCES {
            break;
        }
        sources.push(src);
        added += 1;
    }
    added
}

/// Reciprocal Rank Fusion: merge ranked passage lists into one. A chunk's score
/// is Σ 1/(k + rank) across the lists it appears in (k=60, the standard constant).
/// Robust to the two lists using different score scales (cosine vs ts_rank).
fn rrf_fuse(lists: Vec<Vec<crate::db::chunks::ChunkHit>>, limit: usize) -> Vec<crate::db::chunks::ChunkHit> {
    use std::collections::HashMap;
    const K: f32 = 60.0;
    let mut scores: HashMap<uuid::Uuid, f32> = HashMap::new();
    let mut by_id: HashMap<uuid::Uuid, crate::db::chunks::ChunkHit> = HashMap::new();

    for list in lists {
        for (rank, hit) in list.into_iter().enumerate() {
            *scores.entry(hit.id).or_insert(0.0) += 1.0 / (K + rank as f32 + 1.0);
            by_id.entry(hit.id).or_insert(hit);
        }
    }

    let mut ids: Vec<uuid::Uuid> = by_id.keys().copied().collect();
    ids.sort_by(|a, b| {
        scores[b]
            .partial_cmp(&scores[a])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    ids.truncate(limit);
    ids.into_iter().filter_map(|id| by_id.remove(&id)).collect()
}

/// Group passage hits (already similarity-ordered) back under their source items,
/// preserving rank, capped to `MAX_SOURCES` items and `MAX_PASSAGES_PER_ITEM` each.
fn group_hits(hits: Vec<crate::db::chunks::ChunkHit>) -> Vec<Source> {
    let mut order: Vec<uuid::Uuid> = Vec::new();
    let mut map: std::collections::HashMap<uuid::Uuid, Source> = std::collections::HashMap::new();

    for hit in hits {
        let entry = map.entry(hit.item_id).or_insert_with(|| {
            order.push(hit.item_id);
            Source {
                item: RetrievedItem {
                    id: hit.item_id,
                    url: hit.url.clone(),
                    title: hit.title.clone(),
                    summary: hit.summary.clone(),
                    raw_content: None,
                    tags: Vec::new(),
                    category: None,
                    context_window: None,
                    shared_by: hit.shared_by,
                    shared_by_username: hit.username.clone(),
                    message_id: hit.message_id,
                    shared_at: hit.shared_at,
                    similarity: hit.similarity,
                },
                passages: Vec::new(),
            }
        });
        if entry.passages.len() < MAX_PASSAGES_PER_ITEM {
            entry.passages.push(hit.content);
        }
    }

    order
        .into_iter()
        .take(MAX_SOURCES)
        .filter_map(|id| map.remove(&id))
        .collect()
}

/// Build the Tier 5 context: each source numbered, with its title/sharer and the
/// actual passages that matched — the model reasons over real text, not summaries.
fn build_context(sources: &[Source]) -> String {
    let mut out = String::new();
    for (i, src) in sources.iter().enumerate() {
        let n = i + 1;
        let label = formatter::item_label(&src.item);
        let who = src
            .item
            .shared_by_username
            .as_deref()
            .map(|u| format!("@{u}"))
            .unwrap_or_else(|| "someone".to_string());
        let date = src.item.shared_at.format("%Y-%m-%d");
        let passages = src
            .passages
            .iter()
            .map(|p| format!("   \"…{}…\"", p.trim()))
            .collect::<Vec<_>>()
            .join("\n");

        out.push_str(&format!(
            "[{n}] {label} ({url}) — shared by {who} on {date}\n{passages}\n\n",
            url = src.item.url,
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

    fn answered(raw: &str, n: usize) -> (String, Vec<usize>) {
        match resolve_outcome(raw, n) {
            AnswerOutcome::Answered { answer, cited } => (answer, cited),
            AnswerOutcome::NeedMore { .. } => panic!("expected Answered, got NeedMore"),
        }
    }

    #[test]
    fn strict_schema_with_citations() {
        let (ans, cited) = answered(r#"{"answerable":true,"answer":"hi [2]","sources":[2]}"#, 3);
        assert_eq!(ans, "hi [2]");
        assert_eq!(cited, vec![2]);
    }

    #[test]
    fn wrong_shape_does_not_leak_json() {
        // The bug from the field: model returned {"text": ...}. Must extract the
        // text, never surface raw braces.
        let (ans, cited) = answered(r#"{"text":"some answer"}"#, 5);
        assert_eq!(ans, "some answer");
        assert!(cited.is_empty());
    }

    #[test]
    fn explicit_not_answerable_yields_follow_up() {
        match resolve_outcome(
            r#"{"answerable":false,"answer":"","sources":[],"follow_up":["q1","q2"]}"#,
            3,
        ) {
            AnswerOutcome::NeedMore { follow_up } => assert_eq!(follow_up, vec!["q1", "q2"]),
            AnswerOutcome::Answered { .. } => panic!("expected NeedMore"),
        }
    }

    #[test]
    fn plain_prose_infers_citations() {
        let (ans, cited) = answered("Plain answer drawing on [1] and [3].", 3);
        assert_eq!(ans, "Plain answer drawing on [1] and [3].");
        assert_eq!(cited, vec![1, 3]);
    }

    #[test]
    fn citations_inferred_when_array_missing() {
        let (_, cited) = answered(r#"{"answer":"uses [1] and [2]"}"#, 4);
        assert_eq!(cited, vec![1, 2]);
    }

    #[test]
    fn fenced_json_is_parsed() {
        let (ans, _) = answered("```json\n{\"answerable\":true,\"answer\":\"x\",\"sources\":[]}\n```", 2);
        assert_eq!(ans, "x");
    }

    #[test]
    fn out_of_range_citations_dropped() {
        let cited = cited_in_text("refs [1] [9] [2]", 3);
        assert_eq!(cited, vec![1, 2]);
    }

    fn hit(id: uuid::Uuid) -> crate::db::chunks::ChunkHit {
        crate::db::chunks::ChunkHit {
            id,
            item_id: uuid::Uuid::new_v4(),
            url: "u".into(),
            title: None,
            summary: None,
            shared_by: None,
            username: None,
            message_id: None,
            shared_at: chrono::Utc::now(),
            content: "c".into(),
            similarity: 0.0,
        }
    }

    #[test]
    fn rrf_ranks_shared_chunk_first_and_dedups() {
        let (a, b, c) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        // `a` appears in both lists; `b` and `c` once each.
        let fused = rrf_fuse(vec![vec![hit(a), hit(b)], vec![hit(c), hit(a)]], 10);
        assert_eq!(fused.len(), 3, "duplicates should be merged");
        assert_eq!(fused[0].id, a, "chunk in both lists ranks first");
    }
}
