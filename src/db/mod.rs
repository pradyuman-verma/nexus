//! Database access layer. Thin, hand-written sqlx queries (runtime-checked,
//! so the project builds without a live database at compile time).

pub mod chunks;
pub mod edges;
pub mod entities;
pub mod groups;
pub mod items;
pub mod notifications;
pub mod profiles;

use anyhow::{Context, Result};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use std::time::Duration;

/// Build the connection pool.
pub async fn init_pool(database_url: &str) -> Result<PgPool> {
    PgPoolOptions::new()
        .max_connections(10)
        .acquire_timeout(Duration::from_secs(10))
        .connect(database_url)
        .await
        .context("failed to connect to Postgres")
}

/// Run all migrations in `./migrations`.
pub async fn run_migrations(pool: &PgPool) -> Result<()> {
    sqlx::migrate!("./migrations")
        .run(pool)
        .await
        .context("migration failed")
}

/// Create the pgvector columns + ANN index at the configured dimension.
///
/// The dimension lives in config (it varies by embedding model: 768 for
/// `nomic-embed-text`, 1024 for `mxbai-embed-large`, 1536 for OpenAI), so the
/// DDL can't be a static migration. `dim` is an internal integer — safe to
/// interpolate. Adding a column is a no-op if it already exists; changing the
/// dimension of an existing column requires a fresh database.
pub async fn ensure_vector_schema(pool: &PgPool, dim: usize) -> Result<()> {
    let stmts = [
        format!("ALTER TABLE items ADD COLUMN IF NOT EXISTS embedding vector({dim})"),
        "CREATE INDEX IF NOT EXISTS items_embedding_idx ON items \
         USING ivfflat (embedding vector_cosine_ops) WITH (lists = 100)"
            .to_string(),
        format!(
            "ALTER TABLE user_profiles ADD COLUMN IF NOT EXISTS interest_vector vector({dim})"
        ),
        // chunks table is created by migration 007; its vector column lives here.
        format!("ALTER TABLE chunks ADD COLUMN IF NOT EXISTS embedding vector({dim})"),
        "CREATE INDEX IF NOT EXISTS chunks_embedding_idx ON chunks \
         USING ivfflat (embedding vector_cosine_ops) WITH (lists = 100)"
            .to_string(),
    ];
    for stmt in stmts {
        // `dim` is an internal integer, never user input — safe to execute as
        // raw SQL (we assert past sqlx's dynamic-string injection guard).
        sqlx::raw_sql(sqlx::AssertSqlSafe(stmt.clone()))
            .execute(pool)
            .await
            .with_context(|| format!("vector schema DDL failed: {stmt}"))?;
    }
    Ok(())
}
