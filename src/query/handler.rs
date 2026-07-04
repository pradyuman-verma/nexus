//! Query handler: embed → pgvector search → Tier 5 synthesis → formatted reply.

use crate::bot::formatter;
use crate::db;
use crate::llm::chat::QueryIntent;
use crate::models::RetrievedItem;
use crate::search::WebSnippet;
use crate::state::AppState;
use anyhow::Result;
use uuid::Uuid;

/// HTML reply plus cited item ids (for inline feedback buttons).
pub struct QueryReply {
    pub html: String,
    pub cited_item_ids: Vec<Uuid>,
}

/// Personal brain (all channels) vs this chat only.
#[derive(Debug, Clone, Copy)]
pub enum QueryScope {
    Personal,
    Group(i64),
}

/// Whether /ask may pull live web results (requires TAVILY_API_KEY).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WebMode {
    #[default]
    Off,
    /// Closed corpus first; fetch web when the model flags a gap.
    Augment,
    /// Skip saved context; answer from web only.
    Only,
}

/// Optional reply-to anchor — when the user asks about "this" / replies to a capture.
#[derive(Debug, Clone, Default)]
pub struct QueryAnchor {
    /// Telegram message id of the capture being asked about (from reply_to).
    pub reply_message_id: Option<i64>,
    /// The message containing the question (excluded from "latest URL" scans).
    pub query_message_id: Option<i64>,
}

const SYSTEM_PROMPT_PERSONAL: &str = "You are Nexus — a sharp research analyst for \
this user's personal second brain. Using their saved context across all channels \
(Telegram, WhatsApp, etc.), write an insightful, synthesized answer to the question. \
Think like a researcher, not a search engine:\n\
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
\"needs_external\": false, \
\"external_queries\": [\"web search queries if outside info is required; empty otherwise\"], \
\"follow_up\": [\"search queries to find missing info; empty when answerable\"]}";

const SYSTEM_PROMPT_GROUP: &str = "You are Nexus — a sharp research analyst for \
this Telegram group's shared captures. Using only items shared in this chat, write \
an insightful, synthesized answer to the question. Think like a researcher, not a \
search engine:\n\
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
\"needs_external\": false, \
\"external_queries\": [\"web search queries if outside info is required; empty otherwise\"], \
\"follow_up\": [\"search queries to find missing info; empty when answerable\"]}";

const SYSTEM_PROMPT_DUAL: &str = "You are Nexus — a sharp research analyst. You have \
TWO source blocks: INTERNAL (the user's saved context) and EXTERNAL (live web snippets). \
INTERNAL is ground truth for what this user/group actually saved; EXTERNAL is supplementary \
and may be less trusted — label your reasoning clearly.\n\
- Lead with the core thesis; connect ideas across sources.\n\
- Cite INTERNAL sources as [1], [2], … and EXTERNAL as [W1], [W2], …\n\
- Prefer INTERNAL when both cover the same fact; use EXTERNAL for gaps, recency, or breadth.\n\
- Never invent facts. If neither block can answer, set \"answerable\" to false.\n\
Write plain text — no markdown. Return JSON only: \
{\"answerable\": true|false, \
\"answer\": \"analysis citing [n] and [Wn] as appropriate\", \
\"sources\": [internal numbers used], \
\"web_sources\": [\"W1\", \"W2\" external markers used], \
\"follow_up\": []}";

const WEB_UNAVAILABLE: &str = "Web search isn't configured — add <code>TAVILY_API_KEY</code> to enable <code>--web</code>.";

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
const NO_RESULTS_PERSONAL: &str =
    "Nothing in your saved context covers that yet. Share links, notes, or voice memos and I'll get smarter.";
const NO_RESULTS_GROUP: &str =
    "Nothing shared in this chat covers that yet. Share a link here and I'll index it.";
const STILL_READING: &str =
    "I'm still reading that link — give me about a minute after you share, then ask again.";
const DEICTIC_NO_REFERENT: &str =
    "Not sure which item you mean — reply directly to the link or note you're asking about.";

/// A source item with the passages that matched the query.
struct Source {
    item: RetrievedItem,
    passages: Vec<String>,
}

enum AnchorResolve {
    Ready(RetrievedItem),
    /// URL seen in chat but ingest hasn't finished yet.
    Pending { url: String },
    None,
}

/// Handle a `/ask` / @mention query. Returns the HTML reply to send.
pub async fn handle(
    state: &AppState,
    chat_id: i64,
    user_id: i64,
    scope: QueryScope,
    web: WebMode,
    anchor: QueryAnchor,
    query_text: &str,
) -> Result<QueryReply> {
    let query_text = query_text.trim();
    if query_text.is_empty() {
        return Ok(QueryReply {
            html: "Ask me something — e.g. <code>/ask what have we seen on robotics funding?</code>"
                .to_string(),
            cited_item_ids: vec![],
        });
    }

    if web == WebMode::Only {
        return handle_web_only(state, query_text).await;
    }

    let resolved = resolve_anchor(state, chat_id, user_id, &anchor, query_text).await;
    let deictic = crate::intake::is_deictic_query(query_text);
    let (anchor_item, focused) = match &resolved {
        AnchorResolve::Ready(item) => {
            let focused = anchor.reply_message_id.is_some() || deictic;
            (Some(item.clone()), focused)
        }
        AnchorResolve::Pending { url } => {
            tracing::info!(chat_id, user_id, url = %url, "ask anchor pending ingest");
            return Ok(QueryReply {
                html: format!("{STILL_READING}\n\n<code>{}</code>", esc_url(url)),
                cited_item_ids: vec![],
            });
        }
        AnchorResolve::None if deictic => {
            tracing::info!(chat_id, user_id, "ask deictic without referent");
            return Ok(QueryReply {
                html: DEICTIC_NO_REFERENT.to_string(),
                cited_item_ids: vec![],
            });
        }
        AnchorResolve::None => (None, false),
    };
    if focused {
        tracing::info!(
            chat_id,
            user_id,
            item = ?anchor_item.as_ref().map(|i| i.id),
            reply = ?anchor.reply_message_id,
            "ask anchored"
        );
    }

    let intent = if web == WebMode::Augment && state.web_search.is_some() && !focused {
        state
            .chat
            .classify_query_intent(query_text)
            .await
            .unwrap_or_default()
    } else {
        QueryIntent::default()
    };

    let no_results = match scope {
        QueryScope::Personal => NO_RESULTS_PERSONAL,
        QueryScope::Group(_) => NO_RESULTS_GROUP,
    };

    // Tier 3: embed the query (also the interest signal below).
    let embed_text = if let Some(item) = anchor_item.as_ref() {
        anchor_aware_query(query_text, item)
    } else {
        query_text.to_string()
    };
    let qvec = state.embedder.embed(&embed_text).await?;

    // Querying is the strongest interest signal (weight 3.0).
    let _ = db::events::log(
        &state.pool,
        user_id,
        db::events::event_type::QUERY,
        None,
        3.0,
        serde_json::json!({
            "query": snippet(query_text, 120),
            "chat_id": chat_id,
            "scope": scope_label(scope),
        }),
    )
    .await;
    let _ = db::taste::apply_signal(
        &state.pool,
        user_id,
        &qvec,
        3.0,
        state.config.max_vector_weight,
        state.config.default_relevance_threshold,
        state.config.taste_decay_lambda,
        &[],
        true,
        false,
        true,
    )
    .await;

    // Multi-query / HyDE: expand the question into extra probes + a hypothetical
    // answer, embed them all, and retrieve over the union (failure-tolerant —
    // falls back to just the original query). Skip when anchored — broad expansion
    // pulls in unrelated corpus items.
    let mut embeds = vec![qvec.clone()];
    let extra_probes: Vec<String> = if focused {
        Vec::new()
    } else {
        let expansion = state.chat.expand_query(&embed_text).await.unwrap_or_default();
        expansion
            .probes
            .into_iter()
            .chain(std::iter::once(expansion.hyde))
            .filter(|p| !p.trim().is_empty())
            .collect()
    };
    for probe in &extra_probes {
        if let Ok(e) = state.embedder.embed(probe).await {
            embeds.push(e);
        }
    }
    tracing::info!(
        chat_id,
        user_id,
        scope = scope_label(scope),
        probes = extra_probes.len(),
        query = %snippet(query_text, 80),
        "ask expand"
    );

    let taste = db::taste::get(&state.pool, user_id).await.ok().flatten();
    let system_prompt = build_system_prompt(scope, taste.as_ref(), focused);

    // Retrieve (hybrid + graph), synthesize, and self-correct once if the model
    // says the corpus can't answer and offers follow-up queries.
    let mut keyword_query = embed_text.clone();
    for attempt in 0..2u8 {
        let mut sources = if let Some(item) = anchor_item.as_ref().filter(|_| focused) {
            anchor_sources(state, scope, user_id, item, &embeds).await
        } else {
            let hits = hybrid_retrieve(state, scope, user_id, &keyword_query, &embeds).await;
            group_hits(hits)
        };
        let graph_added = if focused {
            0
        } else {
            let graph_group = match scope {
                QueryScope::Group(g) => g,
                QueryScope::Personal => chat_id,
            };
            expand_with_graph(state, graph_group, query_text, &qvec, &mut sources).await
        };

        if sources.is_empty() {
            if web == WebMode::Augment && state.web_search.is_some() {
                return augment_with_web(
                    state,
                    query_text,
                    &[],
                    vec![query_text.to_string()],
                )
                .await;
            }
            return Ok(QueryReply {
                html: no_results.to_string(),
                cited_item_ids: vec![],
            });
        }
        tracing::info!(chat_id, attempt, sources = sources.len(), graph_added, "ask context");

        let items: Vec<RetrievedItem> = sources.iter().map(|s| s.item.clone()).collect();
        let context = build_context(&sources);
        let user_msg = if focused {
            format!(
                "The user is asking about the specific capture marked [1] (ANCHOR).\n\
                 Sources:\n{context}\n\nQuestion: {query_text}"
            )
        } else {
            format!("Sources:\n{context}\n\nQuestion: {query_text}")
        };
        let raw = state.chat.synthesize(&system_prompt, &user_msg).await?;

        match resolve_outcome(&raw, items.len()) {
            AnswerOutcome::Answered {
                answer,
                cited,
                needs_external,
                external_queries,
            } => {
                if web == WebMode::Augment
                    && state.web_search.is_some()
                    && needs_external
                {
                    let queries = merge_web_queries(
                        query_text,
                        &external_queries,
                        &[],
                        &intent.web_queries,
                    );
                    tracing::info!(chat_id, web_queries = queries.len(), "ask web augment");
                    return augment_with_web(state, query_text, &sources, queries).await;
                }
                tracing::info!(chat_id, attempt, answered = true, cited = cited.len(), "ask answered");
                let cited_item_ids: Vec<Uuid> = cited
                    .iter()
                    .filter_map(|n| items.get(n - 1).map(|i| i.id))
                    .collect();
                return Ok(QueryReply {
                    html: formatter::query_answer(&answer, &items, &cited, &[], &[]),
                    cited_item_ids,
                });
            }
            AnswerOutcome::NeedMore {
                follow_up,
                external_queries,
                ..
            } => {
                if web == WebMode::Augment && state.web_search.is_some() {
                    let queries = merge_web_queries(
                        query_text,
                        &follow_up,
                        &external_queries,
                        &intent.web_queries,
                    );
                    tracing::info!(chat_id, web_queries = queries.len(), "ask web augment");
                    return augment_with_web(state, query_text, &sources, queries).await;
                }
                if attempt >= 1 || follow_up.is_empty() {
                    tracing::info!(chat_id, attempt, answered = false, "ask answered");
                    return Ok(QueryReply {
                        html: no_results.to_string(),
                        cited_item_ids: vec![],
                    });
                }
                tracing::info!(chat_id, follow_ups = follow_up.len(), "ask self-correcting");
                for q in &follow_up {
                    if let Ok(e) = state.embedder.embed(q).await {
                        embeds.push(e);
                    }
                }
                keyword_query = format!("{query_text} {}", follow_up.join(" "));
            }
        }
    }

    Ok(QueryReply {
        html: no_results.to_string(),
        cited_item_ids: vec![],
    })
}

async fn handle_web_only(
    state: &AppState,
    query_text: &str,
) -> Result<QueryReply> {
    let Some(search) = state.web_search.as_ref() else {
        return Ok(QueryReply {
            html: WEB_UNAVAILABLE.to_string(),
            cited_item_ids: vec![],
        });
    };
    let snippets = search.search(query_text).await.unwrap_or_default();
    if snippets.is_empty() {
        return Ok(QueryReply {
            html: "Web search returned no results for that question.".to_string(),
            cited_item_ids: vec![],
        });
    }
    synthesize_dual_reply(state, query_text, &[], &snippets).await
}

async fn augment_with_web(
    state: &AppState,
    query_text: &str,
    sources: &[Source],
    mut queries: Vec<String>,
) -> Result<QueryReply> {
    let Some(search) = state.web_search.as_ref() else {
        return Ok(QueryReply {
            html: WEB_UNAVAILABLE.to_string(),
            cited_item_ids: vec![],
        });
    };
    if queries.is_empty() {
        queries.push(query_text.to_string());
    }
    let cap = state.config.web_search_max_results;
    let snippets = search.search_many(&queries, cap).await.unwrap_or_default();
    if snippets.is_empty() && sources.is_empty() {
        return Ok(QueryReply {
            html: NO_RESULTS_PERSONAL.to_string(),
            cited_item_ids: vec![],
        });
    }
    synthesize_dual_reply(state, query_text, sources, &snippets).await
}

async fn synthesize_dual_reply(
    state: &AppState,
    query_text: &str,
    sources: &[Source],
    snippets: &[WebSnippet],
) -> Result<QueryReply> {
    let items: Vec<RetrievedItem> = sources.iter().map(|s| s.item.clone()).collect();
    let internal = if sources.is_empty() {
        "(none)\n".to_string()
    } else {
        build_context(sources)
    };
    let external = build_web_context(snippets);
    let user_msg = format!(
        "INTERNAL SOURCES (saved context):\n{internal}\n\n\
         EXTERNAL SOURCES (web — supplementary):\n{external}\n\n\
         Question: {query_text}"
    );
    let raw = state.chat.synthesize(SYSTEM_PROMPT_DUAL, &user_msg).await?;
    let DualOutcome {
        answer,
        cited,
        web_cited,
    } = resolve_dual_outcome(&raw, items.len(), snippets.len());
    if answer.trim().is_empty() {
        return Ok(QueryReply {
            html: "Couldn't synthesize an answer from the available sources.".to_string(),
            cited_item_ids: vec![],
        });
    }
    let cited_item_ids: Vec<Uuid> = cited
        .iter()
        .filter_map(|n| items.get(n - 1).map(|i| i.id))
        .collect();
    Ok(QueryReply {
        html: formatter::query_answer(&answer, &items, &cited, snippets, &web_cited),
        cited_item_ids,
    })
}

fn merge_web_queries(
    question: &str,
    a: &[String],
    b: &[String],
    intent_queries: &[String],
) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for q in intent_queries.iter().chain(a.iter()).chain(b.iter()) {
        let q = q.trim();
        if q.is_empty() || !seen.insert(q.to_lowercase()) {
            continue;
        }
        out.push(q.to_string());
    }
    if out.is_empty() {
        out.push(question.to_string());
    }
    out.truncate(3);
    out
}

fn build_web_context(snippets: &[WebSnippet]) -> String {
    let mut out = String::new();
    for (i, s) in snippets.iter().enumerate() {
        let n = i + 1;
        let snippet = snippet(&s.content, 400);
        out.push_str(&format!(
            "[W{n}] {} ({}) — web\n   \"…{snippet}…\"\n\n",
            s.title, s.url
        ));
    }
    out
}

fn resolve_dual_outcome(raw: &str, n_internal: usize, n_web: usize) -> DualOutcome {
    let cleaned = strip_fences(raw);
    let json: Option<serde_json::Value> = serde_json::from_str(cleaned).ok().or_else(|| {
        let (s, e) = (cleaned.find('{'), cleaned.rfind('}'));
        match (s, e) {
            (Some(s), Some(e)) if e > s => serde_json::from_str(&cleaned[s..=e]).ok(),
            _ => None,
        }
    });
    if let Some(v) = json {
        if v.get("answerable").and_then(serde_json::Value::as_bool) == Some(false) {
            return DualOutcome {
                answer: String::new(),
                cited: vec![],
                web_cited: vec![],
            };
        }
        if let Some(answer) = v
            .get("answer")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let cited = sanitize_cited(
                &v.get("sources")
                    .and_then(serde_json::Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(serde_json::Value::as_u64)
                            .map(|x| x as usize)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default(),
                n_internal,
            );
            let cited = if cited.is_empty() {
                cited_in_text(answer, n_internal)
            } else {
                cited
            };
            let explicit_web: Vec<usize> = v
                .get("web_sources")
                .and_then(serde_json::Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|x| {
                            x.as_str()
                                .and_then(parse_web_marker)
                                .or_else(|| x.as_u64().map(|n| n as usize))
                        })
                        .collect()
                })
                .unwrap_or_default();
            let web_cited = sanitize_cited(&explicit_web, n_web);
            let web_cited = if web_cited.is_empty() {
                cited_web_in_text(answer, n_web)
            } else {
                web_cited
            };
            return DualOutcome {
                answer: answer.to_string(),
                cited,
                web_cited,
            };
        }
    }
    let prose = cleaned.trim();
    DualOutcome {
        answer: prose.to_string(),
        cited: cited_in_text(prose, n_internal),
        web_cited: cited_web_in_text(prose, n_web),
    }
}

fn parse_web_marker(s: &str) -> Option<usize> {
    let s = s.trim().trim_start_matches(['[', 'w', 'W']).trim_end_matches(']');
    s.parse().ok()
}

fn cited_web_in_text(text: &str, n: usize) -> Vec<usize> {
    let mut out = Vec::new();
    let upper = text.to_uppercase();
    let mut rest = upper.as_str();
    while let Some(open) = rest.find("[W") {
        rest = &rest[open + 2..];
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

fn scope_label(scope: QueryScope) -> &'static str {
    match scope {
        QueryScope::Personal => "personal",
        QueryScope::Group(_) => "group",
    }
}

fn build_system_prompt(
    scope: QueryScope,
    taste: Option<&crate::models::UserTasteProfile>,
    anchored: bool,
) -> String {
    let base = match scope {
        QueryScope::Personal => SYSTEM_PROMPT_PERSONAL,
        QueryScope::Group(_) => SYSTEM_PROMPT_GROUP,
    };
    let mut out = if anchored {
        format!(
            "{base}\n\nIMPORTANT: The user pointed at a specific capture (ANCHOR, source [1]). \
             Answer primarily about that item. Do not synthesize unrelated sources unless \
             the question explicitly asks for comparison."
        )
    } else {
        base.to_string()
    };
    let Some(t) = taste else {
        return out;
    };
    if t.liked_tags.is_empty() && t.disliked_tags.is_empty() {
        return out;
    }
    let liked = if t.liked_tags.is_empty() {
        "—".to_string()
    } else {
        t.liked_tags.join(", ")
    };
    let disliked = if t.disliked_tags.is_empty() {
        "—".to_string()
    } else {
        t.disliked_tags.join(", ")
    };
    out.push_str(&format!(
        "\n\nUser taste hints (soft guidance, never override the sources): \
         liked topics: {liked}; disliked topics: {disliked}."
    ));
    out
}

async fn resolve_anchor(
    state: &AppState,
    chat_id: i64,
    user_id: i64,
    anchor: &QueryAnchor,
    query_text: &str,
) -> AnchorResolve {
    // 1. Explicit reply-to a message.
    if let Some(mid) = anchor.reply_message_id {
        return resolve_message_target(state, chat_id, mid).await;
    }

    // 2. Deictic ("this", "that") — prefer the most recent URL in chat buffer
    //    over DB recency (the latest link may still be ingesting).
    if crate::intake::is_deictic_query(query_text) {
        if let Ok(urls) = db::groups::recent_urls_from_user(
            &state.pool,
            chat_id,
            user_id,
            anchor.query_message_id,
            12,
        )
        .await
        {
            for (mid, url) in urls {
                if let AnchorResolve::Ready(item) =
                    resolve_message_target(state, chat_id, mid).await
                {
                    return AnchorResolve::Ready(item);
                }
                if let Ok(Some(item)) = db::items::by_url_in_chat(&state.pool, chat_id, &url).await
                {
                    return AnchorResolve::Ready(item);
                }
                return AnchorResolve::Pending { url };
            }
        }

        if let Ok(Some(item)) =
            db::items::latest_in_chat(&state.pool, chat_id, user_id, 30).await
        {
            return AnchorResolve::Ready(item);
        }
        if let Ok(Some(item)) = db::items::latest_in_chat_any(&state.pool, chat_id, 30).await {
            return AnchorResolve::Ready(item);
        }
    }

    AnchorResolve::None
}

async fn resolve_message_target(
    state: &AppState,
    chat_id: i64,
    message_id: i64,
) -> AnchorResolve {
    if let Ok(Some(item)) = db::items::by_message(&state.pool, chat_id, message_id).await {
        return AnchorResolve::Ready(item);
    }
    if let Ok(Some(text)) =
        db::groups::buffer_text(&state.pool, chat_id, message_id).await
    {
        if let Some(url) = crate::intake::extract_urls(&text).into_iter().next() {
            if let Ok(Some(item)) = db::items::by_url_in_chat(&state.pool, chat_id, &url).await {
                return AnchorResolve::Ready(item);
            }
            return AnchorResolve::Pending { url };
        }
    }
    AnchorResolve::None
}

fn esc_url(url: &str) -> String {
    url.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn anchor_aware_query(question: &str, item: &RetrievedItem) -> String {
    let label = item
        .title
        .as_deref()
        .filter(|t| !t.is_empty())
        .unwrap_or(&item.url);
    format!("{question} — specifically about: {label} ({})", item.url)
}

/// Retrieve passages for a single anchored item only.
async fn anchor_sources(
    state: &AppState,
    scope: QueryScope,
    user_id: i64,
    item: &RetrievedItem,
    embeds: &[Vec<f32>],
) -> Vec<Source> {
    let item_ids = [item.id];
    let mut lists = Vec::new();
    match scope {
        QueryScope::Personal => {
            for e in embeds {
                if let Ok(h) = db::chunks::search_within_items_by_owner(
                    &state.pool,
                    user_id,
                    e,
                    &item_ids,
                    CHUNK_K,
                )
                .await
                {
                    lists.push(h);
                }
            }
        }
        QueryScope::Group(group_id) => {
            for e in embeds {
                if let Ok(h) = db::chunks::search_within_items(
                    &state.pool,
                    group_id,
                    e,
                    &item_ids,
                    CHUNK_K,
                )
                .await
                {
                    lists.push(h);
                }
            }
        }
    }
    let hits = rrf_fuse(lists, CHUNK_K as usize);
    if !hits.is_empty() {
        return group_hits(hits);
    }
    // Item ingested but no chunks yet — fall back to summary / raw text.
    let passage = item
        .summary
        .clone()
        .or_else(|| item.raw_content.clone())
        .unwrap_or_else(|| item.url.clone());
    vec![Source {
        item: item.clone(),
        passages: vec![passage],
    }]
}

/// Hybrid retrieval over multiple query embeddings: vector search per embedding
/// + one keyword search, all fused with RRF.
async fn hybrid_retrieve(
    state: &AppState,
    scope: QueryScope,
    user_id: i64,
    keyword_query: &str,
    embeds: &[Vec<f32>],
) -> Vec<crate::db::chunks::ChunkHit> {
    let mut lists = Vec::new();
    match scope {
        QueryScope::Personal => {
            for e in embeds {
                if let Ok(h) = db::chunks::search_by_owner(
                    &state.pool,
                    user_id,
                    e,
                    CHUNK_K,
                    MIN_SIMILARITY,
                )
                .await
                {
                    lists.push(h);
                }
            }
            match db::chunks::keyword_search_by_owner(
                &state.pool,
                user_id,
                keyword_query,
                CHUNK_K,
            )
            .await
            {
                Ok(k) => lists.push(k),
                Err(e) => tracing::debug!(error = %e, "keyword search failed"),
            }
        }
        QueryScope::Group(group_id) => {
            for e in embeds {
                if let Ok(h) =
                    db::chunks::search(&state.pool, group_id, e, CHUNK_K, MIN_SIMILARITY).await
                {
                    lists.push(h);
                }
            }
            match db::chunks::keyword_search(&state.pool, group_id, keyword_query, CHUNK_K).await {
                Ok(k) => lists.push(k),
                Err(e) => tracing::debug!(error = %e, "keyword search failed"),
            }
        }
    }
    rrf_fuse(lists, CHUNK_K as usize)
}

/// Outcome of resolving a Tier 5 response.
enum AnswerOutcome {
    Answered {
        answer: String,
        cited: Vec<usize>,
        needs_external: bool,
        external_queries: Vec<String>,
    },
    NeedMore {
        follow_up: Vec<String>,
        needs_external: bool,
        external_queries: Vec<String>,
    },
}

struct DualOutcome {
    answer: String,
    cited: Vec<usize>,
    web_cited: Vec<usize>,
}

/// Resolve a Tier 5 response into an outcome.
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
        let external_queries = string_array(v, "external_queries");
        let needs_external = v
            .get("needs_external")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);

        // Explicit "not answerable" → ask for more, with the model's leads.
        if v.get("answerable").and_then(serde_json::Value::as_bool) == Some(false) {
            return AnswerOutcome::NeedMore {
                follow_up,
                needs_external,
                external_queries,
            };
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
                needs_external,
                external_queries,
            };
        }
        // JSON but no answer field — don't leak braces; treat as "need more".
        return AnswerOutcome::NeedMore {
            follow_up,
            needs_external,
            external_queries,
        };
    }

    // Not JSON → plain prose answer, infer citations from the text.
    let prose = cleaned.trim();
    if prose.is_empty() || prose.starts_with('{') {
        return AnswerOutcome::NeedMore {
            follow_up: Vec::new(),
            needs_external: false,
            external_queries: Vec::new(),
        };
    }
    AnswerOutcome::Answered {
        answer: prose.to_string(),
        cited: cited_in_text(prose, n),
        needs_external: false,
        external_queries: Vec::new(),
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
                    source_channel: hit.source_channel.clone(),
                    content_type: hit.content_type.clone(),
                    group_id: hit.group_id,
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
            AnswerOutcome::Answered { answer, cited, .. } => (answer, cited),
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
            AnswerOutcome::NeedMore { follow_up, .. } => assert_eq!(follow_up, vec!["q1", "q2"]),
            AnswerOutcome::Answered { .. } => panic!("expected NeedMore"),
        }
    }

    #[test]
    fn needs_external_flag_parsed() {
        match resolve_outcome(
            r#"{"answerable":true,"answer":"x","sources":[1],"needs_external":true,"external_queries":["q"]}"#,
            3,
        ) {
            AnswerOutcome::Answered {
                needs_external,
                external_queries,
                ..
            } => {
                assert!(needs_external);
                assert_eq!(external_queries, vec!["q"]);
            }
            AnswerOutcome::NeedMore { .. } => panic!("expected Answered"),
        }
    }

    #[test]
    fn web_citations_inferred() {
        let out = resolve_dual_outcome("Answer from [W1] and [1].", 2, 2);
        assert_eq!(out.web_cited, vec![1]);
        assert_eq!(out.cited, vec![1]);
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
            source_channel: Some("telegram".into()),
            content_type: Some("article".into()),
            group_id: Some(-100123),
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
