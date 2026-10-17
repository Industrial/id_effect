---
title: 'AUTH8 — Account flows: r10-auth8-accounts'
slug: r10-auth8-accounts
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'Registration, email verification, forgot/reset password; single-use expiring tokens stored as SHA-256, compared in constant time (pattern of `src/apikey.rs`); reset re-hashes through `PasswordHasher` and revokes sessions; forgot-password answers identically for unknown accounts; `AccountNotifier` trait implemented by `id_effect_mail`. Tests `reset_token_is_single_use`, `expired_token_rejected`, `token_stored_hashed`, `forgot_password_response_is_uniform`, `reset_revokes_sessions`.'
  - 'Gate: `<pg> -p id_effect_auth --features accounts`; `-p id_effect_mail --features auth`'
  - 'Owns: `crates/id_effect_auth/src/account/**` (feature `accounts` = `pg` + `axum`), `crates/id_effect_auth/migrations/` (token table), `crates/id_effect_mail/src/auth.rs` (feature `auth`), `book/src/part6/ch30-03-passwords-sessions.md` (section)'
---

# r10-auth8-accounts

Leaf of `.maestro/specs/releases/0.10.0.md`, section "AUTH8 — Account flows (M, wave 2, after `r10-e7-3-transports`) · sec.authn #8 (bonus)". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.
