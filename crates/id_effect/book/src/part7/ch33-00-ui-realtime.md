# Dioxus SSR and realtime

Platform UI is **Dioxus-first** with a thin `id_effect_dioxus` bridge — no heavy UI framework in the default dependency graph.

## SSR bridge

`SsrBridge` runs `Effect` programs and returns HTML fragments for hydration. Wire routes through `id_effect_axum` using the existing channel bridge pattern.

```rust
use id_effect_dioxus::{SsrBridge, SsrRequest, render_effect};
```

Enable optional `dioxus` feature for full component SSR when your app already depends on Dioxus.

## Realtime channels

`RealtimeHub` broadcasts `RealtimeEvent` values to SSE and WebSocket subscribers:

- `SseStream` — `text/event-stream` for browser `EventSource`
- `WebSocketSession` — bidirectional JSON frames

## Forms at the HTTP edge

`FormSubmission` validates `application/x-www-form-urlencoded` and `multipart/form-data` at the Axum boundary, then delegates to schema-backed `Effect` handlers via `decode_form`.

## Leptos alternate

See `docs/platform/LEPTOS_ALTERNATE.md` for teams not adopting Dioxus.
