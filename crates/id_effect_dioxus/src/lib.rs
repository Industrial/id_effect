//! Thin **Dioxus SSR bridge** for `id_effect` — realtime channels and form decoding at the HTTP edge.
//!
//! Default build has **no** Dioxus dependency; enable feature `dioxus` for `dioxus-ssr`.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![allow(clippy::new_ret_no_self, clippy::unused_unit)]

pub mod bridge;
pub mod forms;
pub mod realtime;

pub use bridge::{SsrBridge, SsrRequest, SsrResponse, render_effect};
pub use forms::{FormError, FormField, FormSubmission, decode_form, require_field};
pub use realtime::{
  RealtimeEvent, RealtimeHub, RealtimeTransport, WebSocketSession, sse_handler, websocket_handler,
};
