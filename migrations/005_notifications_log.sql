-- Records every relevance notification sent, enforcing one-per-(user,item).
-- Also stores below-threshold scores when NOTIFICATION_SCORE_LOG is enabled,
-- for future threshold calibration (sent = FALSE).

CREATE TABLE IF NOT EXISTS notifications_log (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id     BIGINT NOT NULL REFERENCES users(id),
    item_id     UUID NOT NULL REFERENCES items(id),
    score       FLOAT NOT NULL,
    sent        BOOLEAN NOT NULL DEFAULT TRUE,
    sent_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (user_id, item_id)
);

CREATE INDEX IF NOT EXISTS notifications_user_idx ON notifications_log (user_id);
