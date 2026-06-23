-- Core relational schema: groups, users, items, entities, edges.
-- Vector columns are added in 002 once the pgvector extension exists.

CREATE TABLE IF NOT EXISTS groups (
    id          BIGINT PRIMARY KEY,           -- Telegram chat_id
    name        TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS users (
    id          BIGINT PRIMARY KEY,           -- Telegram user_id
    username    TEXT,
    first_name  TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS items (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    group_id        BIGINT NOT NULL REFERENCES groups(id),
    shared_by       BIGINT REFERENCES users(id),
    url             TEXT NOT NULL,
    message_id      BIGINT,                    -- original TG message id for deep linking
    title           TEXT,
    raw_content     TEXT,                      -- clean text post-extraction
    summary         TEXT,                      -- Tier 2 summary
    tags            TEXT[] NOT NULL DEFAULT '{}',
    category        TEXT,                      -- Tier 2 classification
    context_window  JSONB,                     -- messages before/after the share — THE MOAT
    source          TEXT NOT NULL DEFAULT 'telegram', -- telegram | twitter | rss | manual
    fetch_status    TEXT NOT NULL DEFAULT 'ok',       -- ok | unavailable | pending_retry
    graph_processed BOOLEAN NOT NULL DEFAULT FALSE,
    shared_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS items_group_shared_idx ON items (group_id, shared_at DESC);
CREATE INDEX IF NOT EXISTS items_unprocessed_idx  ON items (graph_processed) WHERE graph_processed = FALSE;
CREATE INDEX IF NOT EXISTS items_retry_idx        ON items (fetch_status) WHERE fetch_status = 'pending_retry';
-- Dedup: one row per (group, url) per calendar day (UTC).
-- Note: `shared_at::date` / DATE(shared_at) is only STABLE (depends on the
-- session timezone) so Postgres rejects it in an index. Pinning to UTC via
-- `AT TIME ZONE 'UTC'` yields an IMMUTABLE expression.
CREATE UNIQUE INDEX IF NOT EXISTS items_dedup_idx
    ON items (group_id, url, ((shared_at AT TIME ZONE 'UTC')::date));

CREATE TABLE IF NOT EXISTS entities (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    group_id    BIGINT NOT NULL REFERENCES groups(id),
    name        TEXT NOT NULL,
    type        TEXT NOT NULL,     -- person | company | topic | project | technology | fund
    first_seen  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_seen   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (group_id, name, type)
);

CREATE INDEX IF NOT EXISTS entities_group_idx ON entities (group_id);

CREATE TABLE IF NOT EXISTS edges (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    group_id        BIGINT NOT NULL REFERENCES groups(id),
    source_id       UUID NOT NULL,
    source_type     TEXT NOT NULL,   -- 'item' | 'entity'
    target_id       UUID NOT NULL,
    target_type     TEXT NOT NULL,
    relationship    TEXT NOT NULL,   -- mentions | related_to | same_topic | follow_up | contradicts
    strength        FLOAT NOT NULL DEFAULT 1.0,
    metadata        JSONB,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS edges_source_idx ON edges (group_id, source_id);
CREATE INDEX IF NOT EXISTS edges_target_idx ON edges (group_id, target_id);
