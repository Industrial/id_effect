-- The ledger: organisations (the tenants), their users, accounts, journal entries and postings.
-- Every tenant-owned table carries `org_id`; the repositories filter every statement by it.

CREATE TABLE orgs (
  id TEXT PRIMARY KEY CHECK (id ~ '^[a-z0-9][a-z0-9-]{1,31}$'),
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Password users. `role` is one of the three ledger roles; `totp_secret` (base32) is set once
-- the user enrolled a second factor, and `totp_last_step` makes each TOTP code single-use.
CREATE TABLE users (
  id BIGSERIAL PRIMARY KEY,
  org_id TEXT NOT NULL REFERENCES orgs (id),
  username TEXT NOT NULL UNIQUE,
  password_hash TEXT NOT NULL,
  role TEXT NOT NULL CHECK (role IN ('owner', 'accountant', 'viewer')),
  totp_secret TEXT,
  totp_last_step BIGINT,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX users_org_id ON users (org_id);

-- `version` is the optimistic-locking column; `closed_at` is the soft delete.
CREATE TABLE accounts (
  id BIGSERIAL PRIMARY KEY,
  org_id TEXT NOT NULL REFERENCES orgs (id),
  code TEXT NOT NULL,
  name TEXT NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('asset', 'liability', 'equity', 'income', 'expense')),
  owner_id TEXT NOT NULL,
  locked BOOLEAN NOT NULL DEFAULT false,
  version BIGINT NOT NULL DEFAULT 1,
  closed_at TIMESTAMPTZ,
  UNIQUE (org_id, code)
);

CREATE TABLE journal_entries (
  id BIGSERIAL PRIMARY KEY,
  org_id TEXT NOT NULL REFERENCES orgs (id),
  memo TEXT NOT NULL,
  posted_by TEXT NOT NULL,
  posted_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX journal_entries_org_id ON journal_entries (org_id, id);

-- One line of an entry, in minor units: debits positive, credits negative.
CREATE TABLE postings (
  id BIGSERIAL PRIMARY KEY,
  org_id TEXT NOT NULL REFERENCES orgs (id),
  entry_id BIGINT NOT NULL REFERENCES journal_entries (id),
  account_id BIGINT NOT NULL REFERENCES accounts (id),
  amount BIGINT NOT NULL CHECK (amount <> 0)
);

CREATE INDEX postings_org_account ON postings (org_id, account_id);
CREATE INDEX postings_entry ON postings (entry_id);

-- Double entry, enforced by the database as well: at commit, every entry touched by the
-- transaction must sum to zero.
CREATE FUNCTION postings_balance_check() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF (SELECT COALESCE(SUM(amount), 0) FROM postings WHERE entry_id = NEW.entry_id) <> 0 THEN
    RAISE EXCEPTION 'journal entry % does not balance', NEW.entry_id USING ERRCODE = '23514';
  END IF;
  RETURN NULL;
END;
$$;

CREATE CONSTRAINT TRIGGER postings_balance
  AFTER INSERT OR UPDATE ON postings
  DEFERRABLE INITIALLY DEFERRED
  FOR EACH ROW EXECUTE FUNCTION postings_balance_check();

-- API tokens are JWTs; the row records who issued which token id (`jti`) and its revocation.
CREATE TABLE api_tokens (
  id TEXT PRIMARY KEY,
  org_id TEXT NOT NULL REFERENCES orgs (id),
  owner_id TEXT NOT NULL,
  name TEXT NOT NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  revoked_at TIMESTAMPTZ
);

CREATE INDEX api_tokens_owner ON api_tokens (org_id, owner_id);
