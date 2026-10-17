-- The transactional outbox and the inbox (id_effect_messaging::migrator,
-- PgOutbox::ensure_schema). Every statement is idempotent. Times are bound by the application's
-- WallClock, never now().
CREATE TABLE IF NOT EXISTS id_effect_outbox (
  id           bigint      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  topic        text        NOT NULL,
  message_id   text        NOT NULL,
  message_key  text,
  headers      jsonb       NOT NULL DEFAULT '{}',
  payload      bytea       NOT NULL,
  attempts     integer     NOT NULL DEFAULT 0 CHECK (attempts >= 0),
  available_at timestamptz NOT NULL,
  inserted_at  timestamptz NOT NULL,
  sent_at      timestamptz,
  dead_at      timestamptz,
  locked_by    text,
  locked_until timestamptz,
  last_error   text
);

-- Relaying: the rows still to publish, oldest first.
CREATE INDEX IF NOT EXISTS id_effect_outbox_pending_idx
  ON id_effect_outbox (id) WHERE sent_at IS NULL AND dead_at IS NULL;

-- Pruning published rows.
CREATE INDEX IF NOT EXISTS id_effect_outbox_sent_idx
  ON id_effect_outbox (sent_at) WHERE sent_at IS NOT NULL;

-- The inbox: one row per message a consumer group processed, written in the handler's
-- transaction; the primary key is the deduplication.
CREATE TABLE IF NOT EXISTS id_effect_inbox (
  consumer_group text        NOT NULL,
  message_id     text        NOT NULL,
  processed_at   timestamptz NOT NULL,
  PRIMARY KEY (consumer_group, message_id)
);

-- Pruning old inbox rows.
CREATE INDEX IF NOT EXISTS id_effect_inbox_processed_idx ON id_effect_inbox (processed_at);
