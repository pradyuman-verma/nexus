-- Per-user interest vector, scoped per group.
-- Updated on every share, query, or receipt event.

-- Note: the `interest_vector vector(N)` column is added at startup by
-- `db::ensure_vector_schema`, since its dimension depends on the embedding model.
CREATE TABLE IF NOT EXISTS user_profiles (
    user_id             BIGINT NOT NULL REFERENCES users(id),
    group_id            BIGINT NOT NULL REFERENCES groups(id),
    vector_weight       FLOAT NOT NULL DEFAULT 0.0,
    relevance_threshold FLOAT NOT NULL DEFAULT 0.72,
    top_tags            TEXT[] NOT NULL DEFAULT '{}', -- rolling tag history for excerpt prompts
    muted_until         TIMESTAMPTZ,          -- relevance notifications paused until this time
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (user_id, group_id)
);

CREATE INDEX IF NOT EXISTS user_profiles_group_idx ON user_profiles (group_id);
