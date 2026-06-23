-- pgvector extension. The dimensioned `embedding` / `interest_vector` columns
-- and the ANN index are created at startup by `db::ensure_vector_schema`,
-- because the dimension depends on the configured embedding model
-- (768 for nomic-embed-text, 1024 for mxbai-embed-large, 1536 for OpenAI).

CREATE EXTENSION IF NOT EXISTS vector;
