-- Passage-level chunks for retrieval. Each item is split into overlapping
-- passages so /ask can search and quote the actual full content, not a summary.
-- The `embedding vector(N)` column + ANN index are added at startup by
-- db::ensure_vector_schema (dimension depends on the embedding model).

CREATE TABLE IF NOT EXISTS chunks (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    item_id     UUID NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    group_id    BIGINT NOT NULL REFERENCES groups(id),
    chunk_index INT NOT NULL,
    content     TEXT NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS chunks_item_idx  ON chunks (item_id);
CREATE INDEX IF NOT EXISTS chunks_group_idx ON chunks (group_id);
