-- Multi-channel identity layer. Nexus is a context capture protocol with
-- many ingress channels (telegram, whatsapp, later instagram/twitter).
-- Internal ids stay BIGINT everywhere; each channel maps its native id
-- space onto them via (channel, external_id). Telegram rows keep their
-- native ids; other channels draw synthetic ids from a sequence parked
-- far above Telegram's id range.

ALTER TABLE groups ADD COLUMN IF NOT EXISTS channel     TEXT NOT NULL DEFAULT 'telegram';
ALTER TABLE groups ADD COLUMN IF NOT EXISTS external_id TEXT;
UPDATE groups SET external_id = id::text WHERE external_id IS NULL;
ALTER TABLE groups ALTER COLUMN external_id SET NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS groups_channel_external_idx
    ON groups (channel, external_id);

ALTER TABLE users ADD COLUMN IF NOT EXISTS channel     TEXT NOT NULL DEFAULT 'telegram';
ALTER TABLE users ADD COLUMN IF NOT EXISTS external_id TEXT;
UPDATE users SET external_id = id::text WHERE external_id IS NULL;
ALTER TABLE users ALTER COLUMN external_id SET NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS users_channel_external_idx
    ON users (channel, external_id);

-- Telegram ids are < ~1e13 today; 9e15 leaves headroom on both sides
-- while staying far under i64::MAX (~9.2e18).
CREATE SEQUENCE IF NOT EXISTS synthetic_id_seq START WITH 9000000000000000;

-- Webhook channels (WhatsApp, later IG/Twitter) redeliver events; first
-- insert wins, replays are dropped.
CREATE TABLE IF NOT EXISTS channel_events (
    channel     TEXT NOT NULL,
    event_id    TEXT NOT NULL,
    received_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (channel, event_id)
);
