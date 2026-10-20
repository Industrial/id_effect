---
title: 'E11.2 — Zeroized, constant-time secrets and a redacted Session: r11-sec-secrets'
slug: r11-sec-secrets
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'M13: Secret<T: Zeroize> zeroizes its value on drop (crates/id_effect_config/src/secret.rs:16-48 has no Drop today); SecretString and SecretBytes aliases; id_effect_auth Password (crates/id_effect_auth/src/password.rs:110-119) zeroizes on drop. MIGRATION-0.11 section (the new bound).'
  - 'L3: equality on Secret<String> and Secret<Vec<u8>> uses subtle::ConstantTimeEq; the generic PartialEq impl (secret.rs:55-58) is removed (MIGRATION-0.11).'
  - 'M14: Session (crates/id_effect_auth/src/session/mod.rs:105, derived Debug) gets a manual Debug that prints the id prefix, user id and timestamps and only the attribute keys, never values.'
  - 'Regression tests `m13_secret_is_zeroized_on_drop` (a recording Zeroize type), `m13_password_is_zeroized_on_drop`, `m14_session_debug_hides_attr_values_and_tokens`, `l3_secret_eq_uses_constant_time_compare`.'
  - 'Gate: `cargo nextest run -p id_effect_config -p id_effect_auth --all-features`; `cargo deny check` (zeroize, subtle)'
  - 'Owns: `crates/id_effect_config/{src/secret.rs,Cargo.toml,tests/secret.rs}`, `crates/id_effect_auth/{src/password.rs,src/session/mod.rs,Cargo.toml}`'
---

# r11-sec-secrets

Leaf of `.maestro/specs/releases/0.11.0.md`, section "E11.2 — Secure by default" (M13, M14, L3). The
release spec's row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 1. Blocked by: 0.10.1 r101-auth (password.rs argon2 semaphore). r11-sec-auth-state takes
id_effect_auth/Cargo.toml in wave 2.
