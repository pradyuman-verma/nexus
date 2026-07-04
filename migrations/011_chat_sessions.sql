-- Working memory: per-user per-chat conversation state for multi-turn /ask.

CREATE TABLE IF NOT EXISTS chat_sessions (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id      BIGINT NOT NULL REFERENCES users(id),
    chat_id      BIGINT NOT NULL REFERENCES groups(id),
    thread_state JSONB NOT NULL DEFAULT '{}',
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (user_id, chat_id)
);

CREATE INDEX IF NOT EXISTS chat_sessions_user_chat_idx
    ON chat_sessions (user_id, chat_id);

CREATE TABLE IF NOT EXISTS session_turns (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    session_id     UUID NOT NULL REFERENCES chat_sessions(id) ON DELETE CASCADE,
    role           TEXT NOT NULL CHECK (role IN ('user', 'assistant')),
    text           TEXT NOT NULL,
    item_ids       UUID[] NOT NULL DEFAULT '{}',
    cited_item_ids UUID[] NOT NULL DEFAULT '{}',
    created_at     TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS session_turns_session_time_idx
    ON session_turns (session_id, created_at DESC);
