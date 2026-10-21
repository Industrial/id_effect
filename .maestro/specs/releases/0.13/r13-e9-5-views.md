---
title: 'E9.5 — Server-rendered UI in id_effect_axum: maud views, components, layouts, htmx, forms: r13-e9-5-views'
slug: r13-e9-5-views
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'D3: id_effect_axum feature `ui` (declared once in crates/id_effect_axum/Cargo.toml with the exact-pinned maud dependency); web.ui #1 maud `Markup` is a response type on typed routes; #2 `Component` (props struct + slot children, composable); #6 `Layout` and partials, the full page or the fragment chosen by the `HX-Request` header.'
  - 'htmx helpers: `HxRequest` / `HxTarget` extractors, `HxTrigger`, `HxRedirect` (same-origin only), `HxReswap`; htmx 2.0.x vendored once under crates/id_effect_axum/assets/ with its sha384 and 0BSD license in assets/README.md, served same-origin by `ui::htmx::asset_route()` with CSP `script-src ''self''`, `htmx.config.allowEval = false` and `selfRequestsOnly = true` set by the layout.'
  - 'web.ui #5 (margin): form helpers bound to `Schema` decode errors (field → message), re-rendering the form with values and errors; translatable message keys (i18n renders them in r13-e9-4-i18n-web).'
  - 'Secure by default (ADR 0013 R9): maud escapes all interpolation; raw HTML only through `ui::trusted_html` (a reviewed allowlist), checked by a grep test over crates/id_effect_axum/src and crates/id_effect_admin/src; htmx requests carry the 0.11 masked CSRF token header; `HxRedirect` refuses another origin. Tests `ui_escapes_untrusted_by_default`, `no_preescaped_outside_trusted_module`, `r9_htmx_requests_carry_masked_token`, `hx_redirect_refuses_foreign_origin`, `htmx_asset_hash_matches_readme`.'
  - 'Functional tests `markup_route_renders_escaped`, `component_slots_render_in_order`, `layout_wraps_page_not_fragment`, `form_errors_bound_to_fields`; example `095_htmx_todo.rs` with `--self-test`; book `part6/ch29-10-server-rendered-ui.md`.'
  - 'Tier: integration (id_effect_axum, no new crate). Coverage: `cargo llvm-cov nextest -p id_effect_axum --features ui --summary-only --fail-under-regions 96`.'
  - 'Gate: `cargo nextest run -p id_effect_axum --features ui`; `cargo run -p id_effect_axum --features ui --example 095_htmx_todo -- --self-test`; `cargo deny check`'
  - 'Owns: `crates/id_effect_axum/Cargo.toml` (feature `ui` and maud), `crates/id_effect_axum/src/ui/**`, `crates/id_effect_axum/assets/{htmx-*.min.js,README.md}`, `crates/id_effect_axum/tests/ui.rs`, `crates/id_effect_axum/examples/095_htmx_todo.rs`, `crates/id_effect/book/src/part6/ch29-10-server-rendered-ui.md`'
---

# r13-e9-5-views

Leaf of `.maestro/specs/releases/0.13.0.md`, section "E9.5 — Server-rendered UI". The release spec's
row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 1, critical path (head of the admin chain). Blocked by: r13-t0-skeletons and r13-f-rcgen (axum
`Cargo.toml`). Blocks r13-e9-3-admin-core, r13-e-benches.
