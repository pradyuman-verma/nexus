//! Global user taste profiles — Layer 2 interest model (one brain per user).

use crate::models::UserTasteProfile;
use crate::scorer::vectors;
use anyhow::Result;
use chrono::{DateTime, Utc};
use pgvector::Vector;
use sqlx::PgPool;

fn row_to_profile(
    user_id: i64,
    vec: Option<Vector>,
    weight: f64,
    threshold: f64,
    liked: Vec<String>,
    disliked: Vec<String>,
    capture_count: i32,
    query_count: i32,
    muted_until: Option<DateTime<Utc>>,
    updated_at: DateTime<Utc>,
) -> UserTasteProfile {
    UserTasteProfile {
        user_id,
        interest_vector: vec.map(|v| v.to_vec()),
        vector_weight: weight as f32,
        notify_threshold: threshold as f32,
        liked_tags: liked,
        disliked_tags: disliked,
        capture_count,
        query_count,
        muted_until,
        updated_at,
    }
}

pub async fn get(pool: &PgPool, user_id: i64) -> Result<Option<UserTasteProfile>> {
    let row: Option<(
        Option<Vector>,
        f64,
        f64,
        Vec<String>,
        Vec<String>,
        i32,
        i32,
        Option<DateTime<Utc>>,
        DateTime<Utc>,
    )> = sqlx::query_as(
        r#"
        SELECT interest_vector, vector_weight, notify_threshold,
               liked_tags, disliked_tags, capture_count, query_count, muted_until,
               updated_at
        FROM user_taste_profiles WHERE user_id = $1
        "#,
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|(v, w, t, liked, disliked, cap, qry, mute, upd)| {
        row_to_profile(user_id, v, w, t, liked, disliked, cap, qry, mute, upd)
    }))
}

/// Users in a group with a taste vector, excluding `exclude_user`.
/// Used by the relevance interrupt inside group chats.
pub async fn list_active_in_group(
    pool: &PgPool,
    group_id: i64,
    exclude_user: i64,
) -> Result<Vec<UserTasteProfile>> {
    let rows: Vec<(
        i64,
        Option<Vector>,
        f64,
        f64,
        Vec<String>,
        Vec<String>,
        i32,
        i32,
        Option<DateTime<Utc>>,
        DateTime<Utc>,
    )> = sqlx::query_as(
        r#"
        SELECT t.user_id, t.interest_vector, t.vector_weight, t.notify_threshold,
               t.liked_tags, t.disliked_tags, t.capture_count, t.query_count,
               t.muted_until, t.updated_at
        FROM user_taste_profiles t
        WHERE t.interest_vector IS NOT NULL
          AND t.user_id <> $2
          AND t.user_id IN (
              SELECT DISTINCT shared_by FROM items
              WHERE group_id = $1 AND shared_by IS NOT NULL
              UNION
              SELECT DISTINCT user_id FROM messages_buffer
              WHERE group_id = $1 AND user_id IS NOT NULL
          )
        "#,
    )
    .bind(group_id)
    .bind(exclude_user)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(uid, v, w, t, liked, disliked, cap, qry, mute, upd)| {
            row_to_profile(uid, v, w, t, liked, disliked, cap, qry, mute, upd)
        })
        .collect())
}

/// Apply a time-decayed weighted update to the user's global interest vector.
pub async fn apply_signal(
    pool: &PgPool,
    user_id: i64,
    item_vector: &[f32],
    signal: f32,
    max_weight: f32,
    default_threshold: f32,
    decay_lambda: f32,
    new_tags: &[String],
    create_if_missing: bool,
    increment_captures: bool,
    increment_queries: bool,
) -> Result<()> {
    let existing = get(pool, user_id).await?;
    if existing.is_none() && !create_if_missing {
        return Ok(());
    }

    let (current_vec, current_weight) = match &existing {
        Some(p) => {
            let decayed = decay_weight(p.vector_weight, p.updated_at, decay_lambda);
            (p.interest_vector.as_deref(), decayed)
        }
        None => (None, 0.0),
    };

    let effective_signal = if signal >= 0.0 { signal } else { 0.0 };
    let (merged_vec, total_weight) = vectors::weighted_update(
        current_vec,
        current_weight,
        item_vector,
        effective_signal,
        max_weight,
    );

    // Negative signals push disliked tags instead of moving the vector.
    let (liked, disliked) = if signal >= 0.0 {
        (
            merge_tags(
                existing.as_ref().map(|p| p.liked_tags.clone()).unwrap_or_default(),
                new_tags,
            ),
            existing
                .as_ref()
                .map(|p| p.disliked_tags.clone())
                .unwrap_or_default(),
        )
    } else {
        (
            existing
                .as_ref()
                .map(|p| p.liked_tags.clone())
                .unwrap_or_default(),
            merge_tags(
                existing
                    .as_ref()
                    .map(|p| p.disliked_tags.clone())
                    .unwrap_or_default(),
                new_tags,
            ),
        )
    };

    let threshold = existing
        .as_ref()
        .map(|p| p.notify_threshold)
        .unwrap_or(default_threshold);

    let vec = Vector::from(merged_vec);

    sqlx::query(
        r#"
        INSERT INTO user_taste_profiles
            (user_id, interest_vector, vector_weight, notify_threshold,
             liked_tags, disliked_tags, capture_count, query_count, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6,
                CASE WHEN $7 THEN 1 ELSE 0 END,
                CASE WHEN $8 THEN 1 ELSE 0 END,
                NOW())
        ON CONFLICT (user_id) DO UPDATE SET
            interest_vector = EXCLUDED.interest_vector,
            vector_weight   = EXCLUDED.vector_weight,
            liked_tags      = EXCLUDED.liked_tags,
            disliked_tags   = EXCLUDED.disliked_tags,
            capture_count   = user_taste_profiles.capture_count
                              + CASE WHEN $7 THEN 1 ELSE 0 END,
            query_count     = user_taste_profiles.query_count
                              + CASE WHEN $8 THEN 1 ELSE 0 END,
            updated_at      = NOW()
        "#,
    )
    .bind(user_id)
    .bind(vec)
    .bind(total_weight as f64)
    .bind(threshold as f64)
    .bind(&liked)
    .bind(&disliked)
    .bind(increment_captures)
    .bind(increment_queries)
    .execute(pool)
    .await?;

    Ok(())
}

fn decay_weight(weight: f32, updated_at: DateTime<Utc>, lambda: f32) -> f32 {
    if weight <= 0.0 || lambda <= 0.0 {
        return weight;
    }
    let days = Utc::now()
        .signed_duration_since(updated_at)
        .num_seconds()
        .max(0) as f32
        / 86_400.0;
    weight * (-lambda * days).exp()
}

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
    threshold: f32,
    default_threshold: f32,
) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO user_taste_profiles (user_id, notify_threshold)
        VALUES ($1, $2)
        ON CONFLICT (user_id) DO UPDATE
        SET notify_threshold = EXCLUDED.notify_threshold, updated_at = NOW()
        "#,
    )
    .bind(user_id)
    .bind(threshold.clamp(0.0, 1.0) as f64)
    .execute(pool)
    .await?;
    let _ = default_threshold;
    Ok(())
}

pub async fn set_mute(
    pool: &PgPool,
    user_id: i64,
    until: Option<DateTime<Utc>>,
) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO user_taste_profiles (user_id, muted_until)
        VALUES ($1, $2)
        ON CONFLICT (user_id) DO UPDATE
        SET muted_until = EXCLUDED.muted_until, updated_at = NOW()
        "#,
    )
    .bind(user_id)
    .bind(until)
    .execute(pool)
    .await?;
    Ok(())
}

/// Personal brain stats for /stats and debugging.
pub async fn stats_for_user(pool: &PgPool, user_id: i64) -> Result<(i64, i32, i32)> {
    let (captures,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM items WHERE owner_user_id = $1",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await?;

    let profile = get(pool, user_id).await?;
    let (cap, qry) = profile
        .map(|p| (p.capture_count, p.query_count))
        .unwrap_or((0, 0));

    Ok((captures, cap, qry))
}

/// If the user dismissed more than `limit` relevance DMs this week, bump threshold.
/// Returns the new threshold when raised.
pub async fn calibrate_threshold(
    pool: &PgPool,
    user_id: i64,
    default_threshold: f32,
    dismiss_limit: i64,
    bump: f32,
) -> Result<Option<f32>> {
    let dismissals =
        crate::db::events::count_since(pool, user_id, crate::db::events::event_type::NOTIFY_DISMISS, 7)
            .await?;
    if dismissals <= dismiss_limit {
        return Ok(None);
    }
    let current = get(pool, user_id)
        .await?
        .map(|p| p.notify_threshold)
        .unwrap_or(default_threshold);
    let new_threshold = (current + bump).min(1.0);
    if (new_threshold - current).abs() < f32::EPSILON {
        return Ok(None);
    }
    set_threshold(pool, user_id, new_threshold, default_threshold).await?;
    Ok(Some(new_threshold))
}

/// Nightly sweep: calibrate threshold for users who dismissed recently.
pub async fn calibrate_all(pool: &PgPool, default_threshold: f32) -> Result<usize> {
    let users = crate::db::events::users_with_event_since(
        pool,
        crate::db::events::event_type::NOTIFY_DISMISS,
        7,
    )
    .await?;
    let mut raised = 0usize;
    for user_id in users {
        if calibrate_threshold(pool, user_id, default_threshold, 3, 0.05)
            .await?
            .is_some()
        {
            raised += 1;
        }
    }
    Ok(raised)
}
