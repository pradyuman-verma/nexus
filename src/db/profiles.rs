//! User interest-profile persistence and weighted vector updates.

use crate::models::UserProfile;
use crate::scorer::vectors;
use anyhow::Result;
use chrono::{DateTime, Utc};
use pgvector::Vector;
use sqlx::PgPool;

fn row_to_profile(
    user_id: i64,
    group_id: i64,
    vec: Option<Vector>,
    weight: f64,
    threshold: f64,
    top_tags: Vec<String>,
    muted_until: Option<DateTime<Utc>>,
) -> UserProfile {
    UserProfile {
        user_id,
        group_id,
        interest_vector: vec.map(|v| v.to_vec()),
        vector_weight: weight as f32,
        relevance_threshold: threshold as f32,
        top_tags,
        muted_until,
    }
}

pub async fn get(pool: &PgPool, user_id: i64, group_id: i64) -> Result<Option<UserProfile>> {
    let row: Option<(Option<Vector>, f64, f64, Vec<String>, Option<DateTime<Utc>>)> =
        sqlx::query_as(
            r#"
            SELECT interest_vector, vector_weight, relevance_threshold, top_tags, muted_until
            FROM user_profiles WHERE user_id = $1 AND group_id = $2
            "#,
        )
        .bind(user_id)
        .bind(group_id)
        .fetch_optional(pool)
        .await?;

    Ok(row.map(|(v, w, t, tags, m)| row_to_profile(user_id, group_id, v, w, t, tags, m)))
}

/// All profiles in a group that have an interest vector, excluding `exclude_user`.
pub async fn list_active_in_group(
    pool: &PgPool,
    group_id: i64,
    exclude_user: i64,
) -> Result<Vec<UserProfile>> {
    let rows: Vec<(i64, Option<Vector>, f64, f64, Vec<String>, Option<DateTime<Utc>>)> =
        sqlx::query_as(
            r#"
            SELECT user_id, interest_vector, vector_weight, relevance_threshold, top_tags, muted_until
            FROM user_profiles
            WHERE group_id = $1 AND user_id <> $2 AND interest_vector IS NOT NULL
            "#,
        )
        .bind(group_id)
        .bind(exclude_user)
        .fetch_all(pool)
        .await?;

    Ok(rows
        .into_iter()
        .map(|(uid, v, w, t, tags, m)| row_to_profile(uid, group_id, v, w, t, tags, m))
        .collect())
}

/// Apply a weighted update to a user's interest vector, creating the profile if
/// `create_if_missing` is set. Passive signals pass `false` so we never bootstrap
/// a profile from mere presence. Also merges `new_tags` into the rolling tag list.
pub async fn apply_weighted_update(
    pool: &PgPool,
    user_id: i64,
    group_id: i64,
    item_vector: &[f32],
    new_weight: f32,
    max_weight: f32,
    default_threshold: f32,
    new_tags: &[String],
    create_if_missing: bool,
) -> Result<()> {
    let existing = get(pool, user_id, group_id).await?;

    let (merged_vec, total_weight) = match &existing {
        Some(p) => vectors::weighted_update(
            p.interest_vector.as_deref(),
            p.vector_weight,
            item_vector,
            new_weight,
            max_weight,
        ),
        None => {
            if !create_if_missing {
                return Ok(());
            }
            vectors::weighted_update(None, 0.0, item_vector, new_weight, max_weight)
        }
    };

    let threshold = existing
        .as_ref()
        .map(|p| p.relevance_threshold)
        .unwrap_or(default_threshold);

    let merged_tags = merge_tags(existing.map(|p| p.top_tags).unwrap_or_default(), new_tags);
    let vec = Vector::from(merged_vec);

    sqlx::query(
        r#"
        INSERT INTO user_profiles
            (user_id, group_id, interest_vector, vector_weight, relevance_threshold, top_tags, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6, NOW())
        ON CONFLICT (user_id, group_id) DO UPDATE
        SET interest_vector = EXCLUDED.interest_vector,
            vector_weight   = EXCLUDED.vector_weight,
            top_tags        = EXCLUDED.top_tags,
            updated_at      = NOW()
        "#,
    )
    .bind(user_id)
    .bind(group_id)
    .bind(vec)
    .bind(total_weight as f64)
    .bind(threshold as f64)
    .bind(&merged_tags)
    .execute(pool)
    .await?;

    Ok(())
}

/// Keep the most recent ~20 distinct tags, newest first.
fn merge_tags(mut existing: Vec<String>, new_tags: &[String]) -> Vec<String> {
    for t in new_tags.iter().rev() {
        existing.retain(|e| !e.eq_ignore_ascii_case(t));
        existing.insert(0, t.clone());
    }
    existing.truncate(20);
    existing
}

pub async fn set_threshold(
    pool: &PgPool,
    user_id: i64,
    group_id: i64,
    threshold: f32,
    default_threshold: f32,
) -> Result<()> {
    // Ensure a row exists (without a vector) so the threshold sticks.
    sqlx::query(
        r#"
        INSERT INTO user_profiles (user_id, group_id, relevance_threshold)
        VALUES ($1, $2, $3)
        ON CONFLICT (user_id, group_id) DO UPDATE
        SET relevance_threshold = EXCLUDED.relevance_threshold, updated_at = NOW()
        "#,
    )
    .bind(user_id)
    .bind(group_id)
    .bind(threshold.clamp(0.0, 1.0) as f64)
    .execute(pool)
    .await?;
    let _ = default_threshold;
    Ok(())
}

/// Mute (or, with `None`, unmute) relevance notifications for a user.
pub async fn set_mute(
    pool: &PgPool,
    user_id: i64,
    group_id: i64,
    until: Option<DateTime<Utc>>,
) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO user_profiles (user_id, group_id, muted_until)
        VALUES ($1, $2, $3)
        ON CONFLICT (user_id, group_id) DO UPDATE
        SET muted_until = EXCLUDED.muted_until, updated_at = NOW()
        "#,
    )
    .bind(user_id)
    .bind(group_id)
    .bind(until)
    .execute(pool)
    .await?;
    Ok(())
}
