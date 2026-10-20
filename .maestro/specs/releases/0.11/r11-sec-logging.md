---
title: 'E11.2 — Default log redaction and log-forging escapes: r11-sec-logging'
slug: r11-sec-logging
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'M11: a pipeline with no explicit policy applies RedactionPolicy::recommended() — case-insensitive fields password, passwd, secret, token, access_token, refresh_token, id_token, api_key, apikey, authorization, cookie, set-cookie, session, csrf, private_key, client_secret and globs *_token, *_secret, *password*; RedactionPolicy::new() (crates/id_effect_logger/src/redact.rs:59-67, redacts nothing) is deprecated for RedactionPolicy::empty(); RedactionPolicy::dangerous_none() records a posture opt-out. MIGRATION-0.11 section.'
  - 'M12: the tracing-line formatter (pipeline.rs:168-181) escapes CR, LF, other control characters and ANSI escapes in messages and values, escapes spaces and = in keys and quotes values containing spaces; the structured backend (pipeline.rs:294-309) escapes keys.'
  - 'Regression tests `m11_default_pipeline_redacts_common_secret_fields`, `m11_empty_policy_is_explicit`, `m12_newline_in_message_cannot_forge_a_line`, `m12_annotation_key_cannot_inject_fields`, `m12_control_and_ansi_sequences_are_escaped`.'
  - 'Gate: `cargo nextest run -p id_effect_logger --all-features`'
  - 'Owns: `crates/id_effect_logger/src/{redact.rs,pipeline.rs,lib.rs}`, `crates/id_effect_logger/tests/{pipeline.rs,security.rs}`'
---

# r11-sec-logging

Leaf of `.maestro/specs/releases/0.11.0.md`, section "E11.2 — Secure by default" (M11, M12). The
release spec's row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 1. Blocked by: r11-sec-adr0013.
