//! Pure vector math: cosine similarity and the running weighted-centroid update.

/// Cosine similarity in [-1, 1]. Returns 0 for degenerate inputs.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

/// Weighted running centroid:
///   total      = current_weight + new_weight
///   new_vector = (current * current_weight + item * new_weight) / total
///
/// `total` is capped at `max_weight` so early items don't dominate forever.
/// Returns `(new_vector, new_total_weight)`.
pub fn weighted_update(
    current: Option<&[f32]>,
    current_weight: f32,
    item: &[f32],
    new_weight: f32,
    max_weight: f32,
) -> (Vec<f32>, f32) {
    match current {
        None => (item.to_vec(), new_weight.min(max_weight)),
        Some(cur) if cur.len() == item.len() => {
            let total = current_weight + new_weight;
            if total <= 0.0 {
                return (item.to_vec(), new_weight.min(max_weight));
            }
            let merged: Vec<f32> = cur
                .iter()
                .zip(item.iter())
                .map(|(c, i)| (c * current_weight + i * new_weight) / total)
                .collect();
            (merged, total.min(max_weight))
        }
        // Dimension mismatch — replace rather than corrupt.
        Some(_) => (item.to_vec(), new_weight.min(max_weight)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_identical() {
        let v = vec![1.0, 2.0, 3.0];
        assert!((cosine_similarity(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_orthogonal() {
        assert!(cosine_similarity(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
    }

    #[test]
    fn update_from_empty_takes_item() {
        let (v, w) = weighted_update(None, 0.0, &[1.0, 1.0], 2.0, 100.0);
        assert_eq!(v, vec![1.0, 1.0]);
        assert_eq!(w, 2.0);
    }

    #[test]
    fn update_caps_weight() {
        let (_, w) = weighted_update(Some(&[1.0]), 99.0, &[2.0], 5.0, 100.0);
        assert_eq!(w, 100.0);
    }
}
