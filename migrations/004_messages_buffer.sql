-- Rolling buffer used to reconstruct an item's context_window.
-- Rows older than 48h are deleted by the daily cleanup cron.

CREATE TABLE IF NOT EXISTS messages_buffer (
    id          BIGSERIAL PRIMARY KEY,
    group_id    BIGINT NOT NULL REFERENCES groups(id),
    user_id     BIGINT REFERENCES users(id),
    username    TEXT,
    message_id  BIGINT NOT NULL,
    text        TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS messages_buffer_group_idx ON messages_buffer (group_id, created_at DESC);
CREATE INDEX IF NOT EXISTS messages_buffer_msgid_idx ON messages_buffer (group_id, message_id);
