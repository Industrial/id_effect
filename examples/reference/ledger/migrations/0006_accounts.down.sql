DELETE FROM users WHERE org_id IS NULL OR role = 'operator';
ALTER TABLE users DROP CONSTRAINT users_role_check;
ALTER TABLE users ADD CONSTRAINT users_role_check CHECK (role IN ('owner', 'accountant', 'viewer'));
ALTER TABLE users ALTER COLUMN org_id SET NOT NULL;
ALTER TABLE users DROP COLUMN email_verified;
ALTER TABLE users DROP COLUMN email;
DROP TABLE IF EXISTS id_effect_account_tokens;
