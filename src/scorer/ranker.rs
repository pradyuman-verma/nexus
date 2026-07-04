//! Multi-signal ranking helpers for Layer 2 taste-aware surfaces.

use crate::models::UserTasteProfile;
use crate::scorer::vectors::cosine_similarity;

/// Combine semantic match with tag affinity and recency for relevance ranking.
pub fn relevance_score(
    item_embedding: &[f32],
    item_tags: &[String],
    shared_at: chrono::DateTime<chrono::Utc>,
    profile: &UserTasteProfile,
) -> f32 {
    let semantic = profile
        .interest_vector
        .as_ref()
        .map(|v| cosine_similarity(item_embedding, v))
        .unwrap_or(0.0);

    let tag_boost = tag_affinity(item_tags, &profile.liked_tags, &profile.disliked_tags);
    let recency = recency_boost(shared_at);

    (semantic * 0.7 + tag_boost * 0.2 + recency * 0.1).clamp(-1.0, 1.0)
}

fn tag_affinity(item_tags: &[String], liked: &[String], disliked: &[String]) -> f32 {
    let mut score = 0.0f32;
    for t in item_tags {
        if liked.iter().any(|l| l.eq_ignore_ascii_case(t)) {
            score += 0.15;
        }
        if disliked.iter().any(|d| d.eq_ignore_ascii_case(t)) {
            score -= 0.25;
        }
    }
    score.clamp(-0.5, 0.5)
}

/// Boost items from the last 7 days; fade over ~90 days.
fn recency_boost(shared_at: chrono::DateTime<chrono::Utc>) -> f32 {
    let days = chrono::Utc::now()
        .signed_duration_since(shared_at)
        .num_days()
        .max(0) as f32;
    if days <= 7.0 {
        0.3
    } else {
        (0.3 * (-0.03 * (days - 7.0)).exp()).max(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn recency_fresh_beats_stale() {
        let fresh = recency_boost(Utc::now());
        let stale = recency_boost(Utc::now() - chrono::Duration::days(60));
        assert!(fresh > stale);
    }
}
