# id_effect_dioxus

Thin bridge between `id_effect` domain `Effect` programs and Dioxus SSR / Axum realtime routes.

- **Default build:** no Dioxus dependency — `SsrBridge` renders placeholder HTML and documents the integration contract.
- **`dioxus` feature:** enables `dioxus-ssr` for real component rendering.

See book chapter 33 and `docs/platform/LEPTOS_ALTERNATE.md`.
