//! Slash-command handling: /ask /stats /threshold /help /mute /unmute.

use crate::bot::formatter::esc;
use crate::graph::builder;
use crate::{db, query};
use crate::state::AppState;
use anyhow::Result;
use chrono::{DateTime, Utc};
use teloxide::prelude::*;
use teloxide::types::ParseMode;

const HELP: &str = "<b>Nexus</b> — your group's collective memory.\n\n\
I silently watch links shared here, build a shared knowledge graph, and DM you \
when something genuinely relevant to your interests lands.\n\n\
<b>Commands</b>\n\
/ask [question] — query everything we've seen\n\
/stats — what I've ingested for this group\n\
/threshold [0.0-1.0] — tune your notification sensitivity (lower = more pings)\n\
/mute — pause your notifications for 24h\n\
/unmute — resume notifications\n\
/ping — quick health check\n\
/help — this message\n\n\
You can also just @mention me with a question.";

/// If `text` is a command, handle it and return true. The reply is sent here.
pub async fn try_handle(
    state: &AppState,
    chat_id: i64,
    user_id: i64,
    reply_to: i32,
    text: &str,
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
        "help" | "start" => HELP.to_string(),
        "ask" => query::handler::handle(state, chat_id, user_id, args).await?,
        "stats" => stats(state, chat_id).await?,
        "threshold" => threshold(state, chat_id, user_id, args).await?,
        "mute" => mute(state, chat_id, user_id, true).await?,
        "unmute" => mute(state, chat_id, user_id, false).await?,
        "ping" => ping(state, chat_id).await,
        "buildgraph" => {
            // Admin/test helper: build the graph now instead of waiting for the 6h cron.
            let _ = state
                .bot
                .send_message(ChatId(chat_id), "🧠 Building the knowledge graph from unprocessed items… this can take a moment.")
                .await;
            buildgraph(state, chat_id).await?
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

/// Lightweight health check: confirms the bot is alive and the DB is reachable,
/// and shows the active models.
async fn ping(state: &AppState, chat_id: i64) -> String {
    let db_line = match sqlx::query_as::<_, (i64,)>(
        "SELECT COUNT(*) FROM items WHERE group_id = $1",
    )
    .bind(chat_id)
    .fetch_one(&state.pool)
    .await
    {
        Ok((n,)) => format!("DB: 🟢 ok ({n} items here)"),
        Err(_) => "DB: 🔴 unreachable".to_string(),
    };

    format!(
        "🟢 <b>Nexus is up.</b>\n{db_line}\nChat: <code>{}</code>\nEmbeddings: <code>{}</code>",
        esc(&state.config.ollama_chat_model),
        esc(&state.config.embedding_model),
    )
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

async fn threshold(state: &AppState, chat_id: i64, user_id: i64, args: &str) -> Result<String> {
    let Ok(value) = args.trim().parse::<f32>() else {
        return Ok("Usage: <code>/threshold 0.65</code> (0.0–1.0, lower = more pings).".to_string());
    };
    if !(0.0..=1.0).contains(&value) {
        return Ok("Threshold must be between 0.0 and 1.0.".to_string());
    }
    db::profiles::set_threshold(
        &state.pool,
        user_id,
        chat_id,
        value,
        state.config.default_relevance_threshold,
    )
    .await?;
    Ok(format!(
        "Done. I'll DM you when something scores <b>{:.0}%</b> or higher against your interests.",
        value * 100.0
    ))
}

async fn mute(state: &AppState, chat_id: i64, user_id: i64, mute: bool) -> Result<String> {
    let until = if mute {
        Some(Utc::now() + chrono::Duration::hours(24))
    } else {
        None
    };
    db::profiles::set_mute(&state.pool, user_id, chat_id, until).await?;
    Ok(if mute {
        "Muted for 24h. I won't send you relevance pings until then. <code>/unmute</code> to resume.".to_string()
    } else {
        "Unmuted. Relevance notifications are back on.".to_string()
    })
}

async fn stats(state: &AppState, chat_id: i64) -> Result<String> {
    let (total,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM items WHERE group_id = $1")
        .bind(chat_id)
        .fetch_one(&state.pool)
        .await?;

    if total == 0 {
        return Ok("Nothing ingested yet. Share some links and I'll start building.".to_string());
    }

    let range: (Option<DateTime<Utc>>, Option<DateTime<Utc>>) =
        sqlx::query_as("SELECT MIN(shared_at), MAX(shared_at) FROM items WHERE group_id = $1")
            .bind(chat_id)
            .fetch_one(&state.pool)
            .await?;

    let tags: Vec<(String, i64)> = sqlx::query_as(
        r#"
        SELECT tag, COUNT(*) AS c
        FROM items, unnest(tags) AS tag
        WHERE group_id = $1
        GROUP BY tag ORDER BY c DESC LIMIT 5
        "#,
    )
    .bind(chat_id)
    .fetch_all(&state.pool)
    .await?;

    let notifications = db::notifications::count_for_group(&state.pool, chat_id)
        .await
        .unwrap_or(0);

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
        "<b>Nexus — group stats</b>\n\n\
         Items ingested: <b>{total}</b>\n\
         Date range: {span}\n\
         Top tags: {top_tags}\n\
         Relevance pings sent: <b>{notifications}</b>"
    ))
}
