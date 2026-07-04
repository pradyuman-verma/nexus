//! Slash-command handling: /ask /stats /threshold /help /mute /unmute.

use crate::bot::formatter::{self, esc};
use crate::graph::builder;
use crate::query::handler::{QueryAnchor, QueryReply, QueryScope, WebMode};
use crate::state::AppState;
use crate::{db, query};
use anyhow::Result;
use chrono::{DateTime, Utc};
use std::time::Duration;
use teloxide::prelude::*;
use teloxide::types::{ChatAction, ParseMode};

const HELP: &str = "<b>Nexus</b> — your personal second brain.\n\n\
<b>Capture</b> (DM): send links, text notes, voice memos, or photos — no commands needed.\n\
<b>Groups</b>: links are captured passively; notes/voice/photos need an @mention.\n\n\
<b>Commands</b>\n\
/ask [question] — search your personal brain (all channels)\n\
/ask --here [question] — search only this chat's captures\n\
/ask --web [question] — your brain + live web when needed\n\
/ask --web-only [question] — web search only (no saved context)\n\
/stats — captures + taste profile\n\
/taste — liked/disliked tags, threshold, signal counts\n\
/threshold [0.0-1.0] — tune relevance DM sensitivity (lower = more pings)\n\
/mute — pause relevance DMs for 24h\n\
/unmute — resume relevance DMs\n\
/ping — status and capabilities\n\
/help — this message\n\n\
You can also @mention me with a question in groups.";

const START_DM: &str = "<b>Welcome to Nexus</b> 👋\n\n\
I'm your personal second brain. Here's how I work:\n\n\
<b>What I capture</b>\n\
• Links — articles, videos, anything with a URL\n\
• Text notes — just message me\n\
• Voice memos — transcribed automatically\n\
• Photos — described and saved\n\n\
Everything you send here (and on WhatsApp, if connected) builds <b>one brain</b> — \
searchable across all channels.\n\n\
<b>Ask questions</b>\n\
<code>/ask what did I save about robotics?</code> — your full brain\n\
<code>/ask --here what links did we share?</code> — this chat only\n\
<code>/ask --web latest news on topic X</code> — brain + web\n\
Or just ask naturally in DM — I'll tell notes from questions.\n\n\
<b>Relevance DMs</b>\n\
When something shared in a group matches your interests, I can DM you. \
You need to have sent <code>/start</code> here at least once so Telegram lets me reach you.\n\n\
<b>Groups</b>\n\
Add me to a group with <b>Privacy Mode off</b> (BotFather → Bot Settings → \
Group Privacy → Turn off) so I can read shared links. \
For notes, voice, or photos in groups, @mention me.\n\n\
Try sharing a link or ask me something with <code>/ask</code>.";

fn start_group(bot_username: &str) -> String {
    format!(
        "<b>Nexus</b> — your group's ambient memory.\n\n\
         I passively capture <b>links</b> shared here and answer questions when @mentioned.\n\
         Use <code>/ask --here …</code> to search this chat; <code>/ask …</code> searches your personal brain.\n\
         Add <code>--web</code> to pull live web results when your saves aren't enough.\n\n\
         For notes, voice memos, and photos, @mention me or \
         <a href=\"https://t.me/{bot_username}\">DM me directly</a>.\n\n\
         <code>/help</code> for commands."
    )
}

/// Parsed `/ask` flags + question text.
pub struct AskRequest {
    pub scope: QueryScope,
    pub web: WebMode,
    pub question: String,
}

/// Parse `/ask [--here] [--web-only|--web] question`.
pub fn parse_ask_args(chat_id: i64, args: &str) -> AskRequest {
    let mut scope = QueryScope::Personal;
    let mut web = WebMode::Off;
    let mut rest = args.trim();

    loop {
        if let Some(r) = rest.strip_prefix("--here") {
            scope = QueryScope::Group(chat_id);
            rest = r.trim();
            continue;
        }
        if let Some(r) = rest.strip_prefix("--web-only") {
            web = WebMode::Only;
            rest = r.trim();
            continue;
        }
        if let Some(r) = rest.strip_prefix("--web") {
            web = WebMode::Augment;
            rest = r.trim();
            continue;
        }
        break;
    }

    AskRequest {
        scope,
        web,
        question: rest.to_string(),
    }
}

/// Run a query with a typing indicator refreshed until the reply is ready.
pub async fn run_query(
    state: &AppState,
    chat_id: i64,
    user_id: i64,
    scope: QueryScope,
    web: WebMode,
    anchor: QueryAnchor,
    query_text: &str,
) -> Result<QueryReply> {
    let bot = state.bot.clone();
    let typing = tokio::spawn(async move {
        loop {
            let _ = bot
                .send_chat_action(ChatId(chat_id), ChatAction::Typing)
                .await;
            tokio::time::sleep(Duration::from_secs(4)).await;
        }
    });
    let result = query::handler::handle(
        state,
        chat_id,
        user_id,
        scope,
        web,
        anchor,
        query_text,
    )
    .await;
    typing.abort();
    result
}

/// Send a /ask reply with optional 👍/👎 feedback buttons.
pub async fn send_query_reply(state: &AppState, chat_id: i64, reply_to: i32, reply: &QueryReply) {
    let primary = reply.cited_item_ids.first().copied();
    let keyboard = formatter::ask_feedback_keyboard(primary);
    let res = state
        .bot
        .send_message(ChatId(chat_id), &reply.html)
        .parse_mode(ParseMode::Html)
        .link_preview_options(formatter::no_preview())
        .reply_markup(keyboard)
        .reply_parameters(teloxide::types::ReplyParameters::new(
            teloxide::types::MessageId(reply_to),
        ))
        .await;
    if let Err(e) = res {
        tracing::warn!(error = %e, "sending query reply failed");
    }
}

/// If `text` is a command, handle it and return true. The reply is sent here.
pub async fn try_handle(
    state: &AppState,
    chat_id: i64,
    user_id: i64,
    reply_to: i32,
    reply_target_message_id: Option<i64>,
    text: &str,
    in_private: bool,
) -> Result<bool> {
    let trimmed = text.trim_start();
    if !trimmed.starts_with('/') {
        return Ok(false);
    }

    // Split "/cmd@bot args" → (cmd, args).
    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let head = parts.next().unwrap_or("");
    let args = parts.next().unwrap_or("").trim();
    let cmd = head
        .trim_start_matches('/')
        .split('@')
        .next()
        .unwrap_or("")
        .to_lowercase();

    tracing::info!(command = %cmd, has_args = !args.is_empty(), chat_id, user_id, "rx command");

    let reply: String = match cmd.as_str() {
        "help" => HELP.to_string(),
        "start" => {
            if in_private {
                START_DM.to_string()
            } else {
                start_group(&state.bot_username)
            }
        }
        "ask" => {
            let req = parse_ask_args(chat_id, args);
            let anchor = QueryAnchor {
                reply_message_id: reply_target_message_id,
                query_message_id: Some(reply_to as i64),
            };
            let reply = run_query(
                state,
                chat_id,
                user_id,
                req.scope,
                req.web,
                anchor,
                &req.question,
            )
            .await?;
            send_query_reply(state, chat_id, reply_to, &reply).await;
            return Ok(true);
        }
        "stats" => stats(state, chat_id, user_id).await?,
        "taste" => taste(state, user_id).await?,
        "threshold" => threshold(state, chat_id, user_id, args).await?,
        "mute" => mute(state, chat_id, user_id, true).await?,
        "unmute" => mute(state, chat_id, user_id, false).await?,
        "ping" => ping(state, user_id).await,
        "buildgraph" => {
            // Admin/test helper: build the graph now instead of waiting for the 6h cron.
            let _ = state
                .bot
                .send_message(ChatId(chat_id), "🧠 Building the knowledge graph from unprocessed items… this can take a moment.")
                .await;
            buildgraph(state, chat_id).await?
        }
        "reindex" => {
            // Backfill passage chunks for items ingested before chunk-level RAG.
            let _ = state
                .bot
                .send_message(
                    ChatId(chat_id),
                    "🔁 Re-indexing existing items into passages…",
                )
                .await;
            reindex(state, chat_id).await?
        }
        _ => return Ok(false), // unknown command — ignore silently
    };

    send(state, chat_id, reply_to, &reply).await;
    Ok(true)
}

async fn send(state: &AppState, chat_id: i64, reply_to: i32, text: &str) {
    let res = state
        .bot
        .send_message(ChatId(chat_id), text)
        .parse_mode(ParseMode::Html)
        .link_preview_options(crate::bot::formatter::no_preview())
        .reply_parameters(teloxide::types::ReplyParameters::new(
            teloxide::types::MessageId(reply_to),
        ))
        .await;
    if let Err(e) = res {
        tracing::warn!(error = %e, "sending command reply failed");
    }
}

/// Health check: DB, personal captures, taste profile, and channel capabilities.
async fn ping(state: &AppState, user_id: i64) -> String {
    let db_line = match sqlx::query("SELECT 1").fetch_one(&state.pool).await {
        Ok(_) => "DB: 🟢 ok".to_string(),
        Err(_) => "DB: 🔴 unreachable".to_string(),
    };

    let (captures, _, _) = db::taste::stats_for_user(&state.pool, user_id)
        .await
        .unwrap_or((0, 0, 0));

    let taste = db::taste::get(&state.pool, user_id).await.ok().flatten();
    let threshold = taste
        .as_ref()
        .map(|p| p.notify_threshold)
        .unwrap_or(state.config.default_relevance_threshold);
    let top_tags = taste
        .as_ref()
        .map(|p| {
            if p.liked_tags.is_empty() {
                "—".to_string()
            } else {
                p.liked_tags
                    .iter()
                    .take(5)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        })
        .unwrap_or_else(|| "—".to_string());
    let muted = taste
        .as_ref()
        .and_then(|p| p.muted_until)
        .map(|u| u > Utc::now())
        .unwrap_or(false);

    let stt = if state.stt.is_some() {
        "🟢 on"
    } else {
        "⚪ off (set STT_API_KEY)"
    };
    let vision = if state.config.anthropic_api_key.is_some() {
        "🟢 on"
    } else {
        "⚪ off (set ANTHROPIC_API_KEY)"
    };
    let whatsapp = if state.config.whatsapp.is_some() {
        "🟢 connected"
    } else {
        "⚪ not configured"
    };
    let web = if state.web_search.is_some() {
        "🟢 on (--web / --web-only)"
    } else {
        "⚪ off (set TAVILY_API_KEY)"
    };

    format!(
        "🟢 <b>Nexus is up.</b>\n\
         {db_line}\n\n\
         <b>Your brain</b>\n\
         Captures: <b>{captures}</b>\n\
         Notify threshold: <b>{:.0}%</b>{}\n\
         Top interests: {top_tags}\n\n\
         <b>Capabilities</b>\n\
         Voice (STT): {stt}\n\
         Photo (vision): {vision}\n\
         WhatsApp: {whatsapp}\n\
         Web search: {web}\n\n\
         Chat model: <code>{}</code>\n\
         Embeddings: <code>{}</code>",
        threshold * 100.0,
        if muted { " · muted" } else { "" },
        esc(&state.config.ollama_chat_model),
        esc(&state.config.embedding_model),
    )
}

/// Backfill passage chunks for items that predate chunk-level RAG.
async fn reindex(state: &AppState, chat_id: i64) -> Result<String> {
    let missing = db::chunks::items_missing_chunks(&state.pool, chat_id, 100).await?;
    let total = missing.len();
    let mut indexed = 0usize;
    let mut passages = 0usize;

    for it in missing {
        let title = it.title.unwrap_or_default();
        let body = it
            .raw_content
            .filter(|s| !s.trim().is_empty())
            .or(it.summary)
            .unwrap_or_default();
        if body.trim().is_empty() {
            continue;
        }
        let owner = it.owner_user_id.unwrap_or(0);
        if owner == 0 {
            continue;
        }
        match crate::ingestion::pipeline::index_chunks(state, it.id, chat_id, owner, &title, &body)
            .await
        {
            Ok(n) if n > 0 => {
                indexed += 1;
                passages += n;
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(item_id = %it.id, error = %e, "reindex failed"),
        }
    }

    Ok(format!(
        "🔁 Re-indexed <b>{indexed}</b>/{total} items into <b>{passages}</b> passages. \
         <code>/ask</code> now reads their full content."
    ))
}

/// Run the Tier 4 graph builder on demand and report what it produced.
async fn buildgraph(state: &AppState, chat_id: i64) -> Result<String> {
    let stats = builder::run(state, state.config.ingestion_batch_size).await?;

    let entities = db::entities::recent(&state.pool, chat_id, 15)
        .await
        .unwrap_or_default();
    let ent_list = if entities.is_empty() {
        "—".to_string()
    } else {
        entities
            .iter()
            .map(|(name, type_)| format!("{name} ({type_})"))
            .collect::<Vec<_>>()
            .join(", ")
    };

    Ok(format!(
        "🧠 <b>Knowledge graph updated</b>\n\n\
         Items processed: <b>{}</b>\n\
         Entities found: <b>{}</b>\n\
         Edges created: <b>{}</b>\n\n\
         <b>Known entities (this group):</b>\n{}",
        stats.items,
        stats.entities,
        stats.edges,
        esc(&ent_list),
    ))
}

async fn threshold(state: &AppState, _chat_id: i64, user_id: i64, args: &str) -> Result<String> {
    let Ok(value) = args.trim().parse::<f32>() else {
        return Ok(
            "Usage: <code>/threshold 0.65</code> (0.0–1.0, lower = more pings).".to_string(),
        );
    };
    if !(0.0..=1.0).contains(&value) {
        return Ok("Threshold must be between 0.0 and 1.0.".to_string());
    }
    db::taste::set_threshold(
        &state.pool,
        user_id,
        value,
        state.config.default_relevance_threshold,
    )
    .await?;
    Ok(format!(
        "Done. I'll DM you when something scores <b>{:.0}%</b> or higher against your interests.",
        value * 100.0
    ))
}

async fn mute(state: &AppState, _chat_id: i64, user_id: i64, mute: bool) -> Result<String> {
    let until = if mute {
        Some(Utc::now() + chrono::Duration::hours(24))
    } else {
        None
    };
    db::taste::set_mute(&state.pool, user_id, until).await?;
    Ok(if mute {
        "Muted for 24h. I won't send you relevance pings until then. <code>/unmute</code> to resume.".to_string()
    } else {
        "Unmuted. Relevance notifications are back on.".to_string()
    })
}

async fn taste(state: &AppState, user_id: i64) -> Result<String> {
    let profile = db::taste::get(&state.pool, user_id).await?;
    let Some(p) = profile else {
        return Ok(
            "No taste profile yet — share captures and ask questions to build one.".to_string(),
        );
    };

    let liked = if p.liked_tags.is_empty() {
        "—".to_string()
    } else {
        p.liked_tags.join(", ")
    };
    let disliked = if p.disliked_tags.is_empty() {
        "—".to_string()
    } else {
        p.disliked_tags.join(", ")
    };

    Ok(format!(
        "<b>Your taste profile</b>\n\n\
         Liked: {liked}\n\
         Disliked: {disliked}\n\n\
         Signals: <b>{}</b> captures, <b>{}</b> queries\n\
         Notify threshold: <b>{:.0}%</b>\n\
         Last updated: {}",
        p.capture_count,
        p.query_count,
        p.notify_threshold * 100.0,
        p.updated_at.format("%Y-%m-%d %H:%M UTC"),
    ))
}

async fn stats(state: &AppState, chat_id: i64, user_id: i64) -> Result<String> {
    let (personal_total, taste_captures, taste_queries) =
        db::taste::stats_for_user(&state.pool, user_id).await?;

    let (group_total,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM items WHERE group_id = $1")
        .bind(chat_id)
        .fetch_one(&state.pool)
        .await?;

    if personal_total == 0 && group_total == 0 {
        return Ok("Nothing captured yet. Share links, notes, voice memos, or photos and I'll start building your brain.".to_string());
    }

    let range: (Option<DateTime<Utc>>, Option<DateTime<Utc>>) =
        sqlx::query_as("SELECT MIN(shared_at), MAX(shared_at) FROM items WHERE owner_user_id = $1")
            .bind(user_id)
            .fetch_one(&state.pool)
            .await?;

    let tags: Vec<(String, i64)> = sqlx::query_as(
        r#"
        SELECT tag, COUNT(*) AS c
        FROM items, unnest(tags) AS tag
        WHERE owner_user_id = $1
        GROUP BY tag ORDER BY c DESC LIMIT 5
        "#,
    )
    .bind(user_id)
    .fetch_all(&state.pool)
    .await?;

    let top_tags = if tags.is_empty() {
        "—".to_string()
    } else {
        tags.iter()
            .map(|(t, _)| t.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };

    let span = match (range.0, range.1) {
        (Some(a), Some(b)) => format!("{} → {}", a.format("%Y-%m-%d"), b.format("%Y-%m-%d")),
        _ => "—".to_string(),
    };

    Ok(format!(
        "<b>Nexus — your brain</b>\n\n\
         Your captures (all channels): <b>{personal_total}</b>\n\
         In this chat: <b>{group_total}</b>\n\
         Date range: {span}\n\
         Top tags: {top_tags}\n\
         Taste signals: {taste_captures} captures, {taste_queries} queries"
    ))
}
