---
title: 'E11.2 — Persistent security state and rotation at login: r11-sec-auth-state'
slug: r11-sec-auth-state
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'M18: PgTotpStepStore (features pg + totp) implements TotpStepStore (crates/id_effect_auth/src/totp.rs:236-240; MemoryTotpStepStore at :251 is the only implementation today) with one atomic compare-and-set statement; migration 0003 creates its table; using MemoryTotpStepStore under the prod profile is reported by the posture check as memory-only security state.'
  - 'A Postgres store for the 0.10.1 LoginThrottle trait (H5), so throttling is shared across instances.'
  - 'L5: the session login path (session/manager.rs) rotates the session id and calls CsrfToken::rotate (from r11-sec-csrf-headers) on successful login.'
  - 'Regression tests `m18_pg_totp_replay_rejected_across_instances`, `m18_pg_totp_concurrent_consume_one_wins`, `pg_login_throttle_shared_across_instances`, `l5_csrf_token_rotates_at_login`, `l5_session_id_rotates_at_login`.'
  - 'Gate: `ID_EFFECT_REQUIRE_PG=1 cargo nextest run -p id_effect_auth --all-features` (Postgres required)'
  - 'Owns: `crates/id_effect_auth/{src/totp.rs,src/session/manager.rs,migrations/0003_*,Cargo.toml,tests/auth_state.rs}`'
---

# r11-sec-auth-state

Leaf of `.maestro/specs/releases/0.11.0.md`, section "E11.2 — Secure by default" (M18, L5). The
release spec's row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 2. Blocked by: 0.10.1 r101-auth (totp.rs lockout, throttle trait), r11-sec-secrets
(id_effect_auth/Cargo.toml) and r11-sec-csrf-headers (CsrfToken::rotate). The throttle file name from
0.10.1 (`throttle.rs`) is added to Owns once that leaf lands.
