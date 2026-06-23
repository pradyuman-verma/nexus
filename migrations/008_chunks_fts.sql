-- Full-text index on chunk content for hybrid (vector + keyword) retrieval.
-- to_tsvector('english', content) with a constant config is IMMUTABLE, so it's
-- indexable. Keyword search catches exact terms / proper nouns / acronyms that
-- semantic embeddings miss.
CREATE INDEX IF NOT EXISTS chunks_fts_idx
    ON chunks USING gin (to_tsvector('english', content));
