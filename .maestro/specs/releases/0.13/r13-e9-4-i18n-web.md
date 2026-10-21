---
title: 'E9.4 — i18n on the web: Accept-Language negotiation, translated validation, i18n in views: r13-e9-4-i18n-web'
slug: r13-e9-4-i18n-web
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'eco.i18n #3: Accept-Language negotiation (fluent-langneg) against the supported-locale allowlist, with a cookie or user-preference override; an axum layer (feature `axum`) puts `CurrentLocale` in the request environment.'
  - 'eco.i18n #5: translated `Schema` validation messages: the error codes of crates/id_effect/src/schema/parse_errors.rs map to message ids; en and de catalogs shipped with the crate (feature `schema`).'
  - 'web.ui #8: feature `maud` implements `maud::Render` for messages, so views call `t(&locale, "id", args)` directly.'
  - 'Secure by default (ADR 0013 R4, R9): the header parse is bounded (≤ 32 ranges, ≤ 256 bytes; beyond that the header is ignored); a locale outside the allowlist falls back to the default; message arguments are HTML-escaped by maud and Fluent bidi isolation stays on. Tests `r4_accept_language_over_limit_ignored`, `locale_outside_allowlist_falls_back`, `i18n_args_escaped_in_html`, `i18n_bidi_isolation_on_by_default`.'
  - 'Functional tests `negotiates_best_supported_locale`, `cookie_override_beats_header`, `validation_errors_translated_de`; example `i18n_web.rs` with `--self-test`; `negotiate::parse_accept_language` public for the fuzz target.'
  - 'Book `part6/ch30-09-i18n.md` gains the web sections (append to the core chapter after r13-e9-4-i18n-core merged).'
  - 'Tier: integration. Coverage: `cargo llvm-cov nextest -p id_effect_i18n --all-features --summary-only --fail-under-regions 96`.'
  - 'Gate: `cargo nextest run -p id_effect_i18n --all-features`; `cargo run -p id_effect_i18n --all-features --example i18n_web -- --self-test`'
  - 'Owns: `crates/id_effect_i18n/src/{negotiate.rs,axum.rs,schema_messages.rs,maud.rs}`, `crates/id_effect_i18n/locales/schema/**`, `crates/id_effect_i18n/tests/{negotiate.rs,web.rs,schema_messages.rs}`, `crates/id_effect_i18n/examples/i18n_web.rs`, the web sections of `crates/id_effect/book/src/part6/ch30-09-i18n.md`'
---

# r13-e9-4-i18n-web

Leaf of `.maestro/specs/releases/0.13.0.md`, section "E9.4 — i18n". The release spec's row is binding;
this file exists so Maestro can track the leaf as its own task.

Wave 2. Blocked by: r13-e9-4-i18n-core (and r13-e9-5-views for the shared maud version). Blocks
r13-e9-3-admin-edit (translated form errors, optional), r13-e-fuzz-security, r13-w5-kit-template.
