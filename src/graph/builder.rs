//! Tier 4 batch entity/edge extraction. Invoked by the 6-hourly cron.

use crate::db::{self, items::GraphItem};
use crate::state::AppState;
use anyhow::Result;
use serde::Deserialize;
use std::collections::HashMap;
use uuid::Uuid;

const RECENT_ENTITY_LIMIT: i64 = 50;

#[derive(Default)]
pub struct GraphStats {
    pub items: usize,
    pub entities: usize,
    pub edges: usize,
}

#[derive(Deserialize, Default)]
struct GraphResponse {
    #[serde(default)]
    entities: Vec<EntityOut>,
    #[serde(default)]
    edges: Vec<EdgeOut>,
    #[serde(default)]
    mentions: Vec<MentionOut>,
}

#[derive(Deserialize)]
struct EntityOut {
    name: String,
    #[serde(rename = "type")]
    type_: String,
}

#[derive(Deserialize)]
struct EdgeOut {
    source: String,
    target: String,
    relationship: String,
    #[serde(default = "one")]
    strength: f32,
}

#[derive(Deserialize)]
struct MentionOut {
    item: usize,
    #[serde(default)]
    entities: Vec<String>,
}

fn one() -> f32 {
    1.0
}

/// Process all unprocessed items across groups, in batches. Returns aggregate stats.
pub async fn run(state: &AppState, batch_size: usize) -> Result<GraphStats> {
    let mut stats = GraphStats::default();

    loop {
        let items = db::items::unprocessed(&state.pool, batch_size as i64).await?;
        if items.is_empty() {
            break;
        }

        // Group the batch by group_id (entities/edges are group-scoped).
        let mut by_group: HashMap<i64, Vec<GraphItem>> = HashMap::new();
        for it in items {
            by_group.entry(it.group_id).or_default().push(it);
        }

        for (group_id, group_items) in by_group {
            match process_group_batch(state, group_id, &group_items).await {
                Ok(s) => {
                    stats.items += s.items;
                    stats.entities += s.entities;
                    stats.edges += s.edges;
                }
                Err(e) => {
                    tracing::warn!(group_id, error = %e, "graph batch failed");
                }
            }
            // Mark processed regardless, so a poisoned batch can't loop forever.
            let ids: Vec<Uuid> = group_items.iter().map(|i| i.id).collect();
            db::items::mark_graph_processed(&state.pool, &ids).await?;
        }
    }

    Ok(stats)
}

async fn process_group_batch(
    state: &AppState,
    group_id: i64,
    items: &[GraphItem],
) -> Result<GraphStats> {
    let existing = db::entities::recent(&state.pool, group_id, RECENT_ENTITY_LIMIT).await?;
    let prompt = build_prompt(items, &existing);

    let raw = state.chat.extract_graph(&prompt).await?;
    let parsed: GraphResponse = parse(&raw)?;

    // Upsert entities → name->id map.
    let mut entity_ids: HashMap<String, Uuid> = HashMap::new();
    let mut stats = GraphStats {
        items: items.len(),
        ..Default::default()
    };

    for e in &parsed.entities {
        if e.name.trim().is_empty() {
            continue;
        }
        let id = db::entities::upsert(&state.pool, group_id, e.name.trim(), &e.type_).await?;
        entity_ids.insert(norm(&e.name), id);
        stats.entities += 1;
    }

    // Resolve an entity by name, upserting an unknown one as a 'topic'.
    async fn resolve(
        state: &AppState,
        group_id: i64,
        ids: &mut HashMap<String, Uuid>,
        name: &str,
    ) -> Result<Uuid> {
        if let Some(id) = ids.get(&norm(name)) {
            return Ok(*id);
        }
        let id = db::entities::upsert(&state.pool, group_id, name.trim(), "topic").await?;
        ids.insert(norm(name), id);
        Ok(id)
    }

    // Entity ↔ entity edges.
    for edge in &parsed.edges {
        if edge.source.trim().is_empty() || edge.target.trim().is_empty() {
            continue;
        }
        let src = resolve(state, group_id, &mut entity_ids, &edge.source).await?;
        let tgt = resolve(state, group_id, &mut entity_ids, &edge.target).await?;
        if src == tgt {
            continue;
        }
        db::edges::insert(
            &state.pool,
            group_id,
            src,
            "entity",
            tgt,
            "entity",
            &edge.relationship,
            edge.strength,
            None,
        )
        .await?;
        stats.edges += 1;
    }

    // Item → entity 'mentions' edges.
    for m in &parsed.mentions {
        // Prompt uses 1-based item indices.
        let Some(item) = m.item.checked_sub(1).and_then(|i| items.get(i)) else {
            continue;
        };
        for name in &m.entities {
            if name.trim().is_empty() {
                continue;
            }
            let ent = resolve(state, group_id, &mut entity_ids, name).await?;
            db::edges::insert(
                &state.pool,
                group_id,
                item.id,
                "item",
                ent,
                "entity",
                "mentions",
                1.0,
                None,
            )
            .await?;
            stats.edges += 1;
        }
    }

    Ok(stats)
}

fn build_prompt(items: &[GraphItem], existing: &[(String, String)]) -> String {
    let mut items_block = String::new();
    for (i, it) in items.iter().enumerate() {
        let n = i + 1;
        let title = it.title.as_deref().unwrap_or("(untitled)");
        let summary = it.summary.as_deref().unwrap_or("");
        let tags = it.tags.join(", ");
        items_block.push_str(&format!(
            "[{n}] {title}\n    summary: {summary}\n    tags: {tags}\n"
        ));
    }

    let existing_block = if existing.is_empty() {
        "(none yet)".to_string()
    } else {
        existing
            .iter()
            .map(|(name, type_)| format!("{name} ({type_})"))
            .collect::<Vec<_>>()
            .join(", ")
    };

    format!(
        "Given these {n} items with their summaries and tags, extract named entities \
         (people, companies, topics, technologies, projects, funds) and relationships \
         between them. Reuse these existing entities where they apply: [{existing_block}].\n\n\
         Items:\n{items_block}\n\
         Return JSON only, no prose, with this shape:\n\
         {{\n\
           \"entities\": [{{\"name\": \"...\", \"type\": \"person|company|topic|project|technology|fund\"}}],\n\
           \"edges\": [{{\"source\": \"EntityName\", \"target\": \"EntityName\", \"relationship\": \"related_to|same_topic|follow_up|contradicts\", \"strength\": 1.0}}],\n\
           \"mentions\": [{{\"item\": 1, \"entities\": [\"EntityName\", \"...\"]}}]\n\
         }}",
        n = items.len(),
    )
}

fn norm(s: &str) -> String {
    s.trim().to_lowercase()
}

fn parse(raw: &str) -> Result<GraphResponse> {
    let cleaned = raw
        .trim()
        .strip_prefix("```json")
        .or_else(|| raw.trim().strip_prefix("```"))
        .unwrap_or(raw.trim())
        .trim_end_matches("```")
        .trim();
    if let Ok(v) = serde_json::from_str::<GraphResponse>(cleaned) {
        return Ok(v);
    }
    let (s, e) = (cleaned.find('{'), cleaned.rfind('}'));
    if let (Some(s), Some(e)) = (s, e) {
        if e > s {
            return Ok(serde_json::from_str::<GraphResponse>(&cleaned[s..=e])?);
        }
    }
    Ok(GraphResponse::default())
}
