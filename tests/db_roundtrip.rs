//! Integration test exercising the full DB layer against a real Postgres+pgvector.
//!
//! Ignored by default (needs a database). Run it explicitly:
//!
//!   docker compose up -d
//!   TEST_DATABASE_URL=postgresql://nexus:nexus@localhost:5432/nexus \
//!     cargo test --test db_roundtrip -- --ignored --nocapture
//!
//! It uses a unique group id per run so it can run repeatedly without cleanup.

use nexus::db::{self, items::NewItem};
use nexus::models::{ContextMessage, ContextPosition, ContextWindow};

const DIM: usize = 8;

fn unit_vec(seed: f32) -> Vec<f32> {
    (0..DIM).map(|i| seed + i as f32 * 0.01).collect()
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL (a live Postgres+pgvector)"]
async fn full_db_roundtrip() {
    let url = std::env::var("TEST_DATABASE_URL")
        .expect("set TEST_DATABASE_URL to a Postgres+pgvector instance");

    let pool = db::init_pool(&url).await.expect("connect");
    db::run_migrations(&pool).await.expect("migrate");
    db::ensure_vector_schema(&pool, DIM)
        .await
        .expect("vector schema");

    // Unique ids per run.
    let now = chrono::Utc::now().timestamp_micros();
    let group_id: i64 = -(1_000_000_000 + (now % 1_000_000));
    let sharer: i64 = 10_000 + (now % 1000);
    let reader: i64 = 20_000 + (now % 1000);

    db::groups::upsert_group(&pool, group_id, Some("test group"))
        .await
        .unwrap();
    db::groups::upsert_user(&pool, sharer, Some("alice"), Some("Alice"))
        .await
        .unwrap();
    db::groups::upsert_user(&pool, reader, Some("bob"), Some("Bob"))
        .await
        .unwrap();

    // Context window (the moat) must survive a round-trip.
    let ctx = ContextWindow {
        messages: vec![ContextMessage {
            user_id: Some(sharer),
            username: Some("alice".into()),
            message_id: 1,
            text: "lol this is exactly what we're building".into(),
            position: ContextPosition::Before,
        }],
        forwarded: false,
        forward_origin: None,
    };

    let emb = unit_vec(0.5);
    let tags = vec!["robotics".to_string(), "funding".to_string()];
    let item_id = db::items::insert(
        &pool,
        NewItem {
            group_id,
            shared_by: sharer,
            url: "https://example.com/robotics-fund",
            message_id: 42,
            title: Some("Robotics megafund"),
            raw_content: Some("A new fund backs robotics startups."),
            summary: Some("A fund backing robotics."),
            tags: &tags,
            category: Some("venture"),
            context_window: &ctx,
            embedding: Some(&emb),
            fetch_status: "ok",
            content_type: "article",
        },
    )
    .await
    .expect("insert")
    .expect("not deduped");

    // Dedup: same url + group same day → no second row.
    let dup = db::items::is_duplicate(&pool, group_id, "https://example.com/robotics-fund", 7)
        .await
        .unwrap();
    assert!(dup, "expected dedup to flag the URL");

    // Sharer profile update (weight 2.0) then a similarity search retrieves it.
    db::profiles::apply_weighted_update(
        &pool, sharer, group_id, &emb, 2.0, 100.0, 0.72, &tags, true,
    )
    .await
    .unwrap();

    let results = db::items::search(&pool, group_id, &unit_vec(0.5), 10, 0.0)
        .await
        .expect("search");
    assert!(!results.is_empty(), "search returned nothing");
    let top = &results[0];
    assert_eq!(top.id, item_id);
    assert_eq!(top.tags, tags);
    assert_eq!(
        top.context_window.as_ref().unwrap().messages[0].text,
        "lol this is exactly what we're building",
        "context_window must round-trip intact"
    );

    // Notification dedup constraint.
    db::notifications::log(&pool, reader, item_id, 0.9, true)
        .await
        .unwrap();
    assert!(db::notifications::already_notified(&pool, reader, item_id)
        .await
        .unwrap());

    // Graph processing flag flips.
    let unprocessed = db::items::unprocessed(&pool, 50).await.unwrap();
    assert!(unprocessed.iter().any(|i| i.id == item_id));
    db::items::mark_graph_processed(&pool, &[item_id])
        .await
        .unwrap();
    let unprocessed2 = db::items::unprocessed(&pool, 50).await.unwrap();
    assert!(!unprocessed2.iter().any(|i| i.id == item_id));
}
