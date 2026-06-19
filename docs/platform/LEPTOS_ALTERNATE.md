# Leptos as an alternate full-stack UI bridge

`id_effect` ships Dioxus-first via `id_effect_dioxus`. Leptos is the documented alternate.

## Wiring

1. Domain logic stays in `Effect<A, E, R>`.
2. Leptos server functions call `id_effect_axum::run_with_env` or `id_effect_tokio::run_async`.
3. Map errors at the HTTP edge; use SSE routes compatible with `id_effect_dioxus::realtime` event shapes.

No `id_effect_leptos` crate in v1 — avoid duplicating the thin bridge pattern.
