-- Self-service accounts (id_effect_auth's `Accounts`, `POST /auth/register`): the single-use
-- verification and reset tokens (a copy of its `migrations/0002_id_effect_account_tokens.up.sql`),
-- then the users' email addresses and the operator role.

-- A token reads `<id>_<secret>`: `id` is its public lookup key, and only the lowercase hex
-- SHA-256 of the secret is stored, so a read of this table yields no usable token.
CREATE TABLE IF NOT EXISTS id_effect_account_tokens (
  id text PRIMARY KEY,
  kind text NOT NULL CHECK (kind IN ('verify_email', 'reset_password')),
  account_id text NOT NULL,
  secret_hash text NOT NULL,
  expires_at timestamptz NOT NULL
);

CREATE INDEX IF NOT EXISTS id_effect_account_tokens_account_idx
  ON id_effect_account_tokens (account_id, kind);

CREATE INDEX IF NOT EXISTS id_effect_account_tokens_expires_at_idx
  ON id_effect_account_tokens (expires_at);

-- A registered user signs in with its (lower-case) address as username once it is verified; it
-- belongs to no organisation until it creates or joins one, so every ledger route refuses it
-- (fail closed). Statements go to the account owner's address.
ALTER TABLE users ADD COLUMN email TEXT UNIQUE;
ALTER TABLE users ADD COLUMN email_verified BOOLEAN NOT NULL DEFAULT false;
ALTER TABLE users ALTER COLUMN org_id DROP NOT NULL;

-- `operator`: the people running the service (`/_console`), not a role inside a ledger.
ALTER TABLE users DROP CONSTRAINT users_role_check;
ALTER TABLE users ADD CONSTRAINT users_role_check
  CHECK (role IN ('owner', 'accountant', 'viewer', 'operator'));
