---
title: 'E9.5 — Leptos SSR + hydration recipe on the id_effect host (not scored): r13-e9-5-leptos'
slug: r13-e9-5-leptos
mode: heavy
work_type: spec-slice
risk_class: low
version: 1
acceptance_criteria:
  - 'D4: examples/recipes/leptos-ssr is a standalone crate outside the workspace (root `exclude` from r13-t0-skeletons, own Cargo.lock): Leptos (exact pin) server-side rendering with hydration, served by the id_effect axum host, calling an id_effect RPC service through the generated Rust client; no npm, no node_modules (the wasm target and the build tool used are recorded in its README).'
  - 'Secure by default: served through the host typed routes (`.public()` for pages, guarded RPC), the 0.11 security headers on; no `dangerous_*` opt-out.'
  - 'Tests `leptos_recipe_ssr_renders`, `leptos_recipe_calls_rpc`; CI job `recipe-leptos` (cached, not in `ci-pass` until green twice).'
  - 'Book `part6/ch29-11-leptos-recipe.md`. web.ui #7 is not claimed (the recipe is outside the published crates).'
  - 'Tier: example (outside the workspace; no coverage gate, as for examples).'
  - 'Gate: `cargo nextest run --manifest-path examples/recipes/leptos-ssr/Cargo.toml`; CI job `recipe-leptos`'
  - 'Owns: `examples/recipes/leptos-ssr/**`, `.github/workflows/ci.yml` (job `recipe-leptos` only), `crates/id_effect/book/src/part6/ch29-11-leptos-recipe.md`'
---

# r13-e9-5-leptos

Leaf of `.maestro/specs/releases/0.13.0.md`, section "E9.5 — Server-rendered UI". The release spec's
row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 3. Blocked by: r13-t0-skeletons (root `exclude`). First in the scope-cut order; moves no row.
