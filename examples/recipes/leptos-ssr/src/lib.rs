//! Recipe: [Leptos](https://leptos.dev) server-side rendering with hydration on the id_effect
//! axum host, calling an id_effect RPC service through its generated Rust client.
//!
//! One crate, two builds:
//!
//! - **`ssr`** (default, the server): [`server`] mounts the Leptos pages and the wasm bundle as
//!   public typed routes and the [`rpc::Notes`] service as guarded procedures on one
//!   `id_effect_axum::route::Router`, behind `SecurityLayer::recommended` (CSRF, a nonce CSP,
//!   the security headers). While rendering, the page loads its data through
//!   [`rpc::NotesClient`] — the client `#[rpc::service]` generated — with a service token the
//!   browser never sees.
//! - **`hydrate`** (`wasm32-unknown-unknown`, the browser): [`hydrate`] attaches the same
//!   [`app::App`] to the server-rendered HTML; the data arrives serialized in the page, so the
//!   browser makes no RPC call of its own.
//!
//! See `README.md` for the build (no npm: `cargo` + `wasm-bindgen-cli`) and the book chapter
//! "Leptos recipe" (part 6) for the walk-through.

#![deny(missing_docs)]

pub mod app;
#[cfg(feature = "ssr")]
pub mod rpc;
#[cfg(feature = "ssr")]
pub mod server;

/// The browser entry point: hydrate the server-rendered `<body>` with [`app::App`]. The hydration
/// script Leptos puts in the page (`HydrationScripts`) calls it once the wasm module is loaded.
#[cfg(feature = "hydrate")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn hydrate() {
  console_error_panic_hook::set_once();
  leptos::mount::hydrate_body(app::App);
}
