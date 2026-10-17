-- What the consumers of `ledger.entry-posted` produce: one balance notification per account an
-- entry touched (written with the inbox row, so a redelivered message adds none), and the
-- statements the monthly job stored.
CREATE TABLE balance_notifications (
  id BIGSERIAL PRIMARY KEY,
  org_id TEXT NOT NULL REFERENCES orgs (id),
  account_id BIGINT NOT NULL REFERENCES accounts (id),
  entry_id BIGINT NOT NULL REFERENCES journal_entries (id),
  balance BIGINT NOT NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  UNIQUE (entry_id, account_id)
);

CREATE INDEX balance_notifications_account ON balance_notifications (org_id, account_id);

-- One row per stored statement: where its CSV is and how it was delivered.
CREATE TABLE statements (
  org_id TEXT NOT NULL REFERENCES orgs (id),
  account_id BIGINT NOT NULL REFERENCES accounts (id),
  month TEXT NOT NULL CHECK (month ~ '^[0-9]{4}-[0-9]{2}$'),
  object_key TEXT NOT NULL,
  bytes BIGINT NOT NULL,
  delivery TEXT NOT NULL CHECK (delivery IN ('attached', 'linked', 'stored')),
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (account_id, month)
);
