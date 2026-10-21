//! Server-rendered UI (feature `ui`): [maud](https://maud.lambda.xyz/) views, components,
//! layouts and partials, htmx helpers and HTML forms bound to `Schema` errors (ADR 0015 D3).
//!
//! | Need | API |
//! |---|---|
//! | Render HTML, escaped by default | [`html!`] → [`Markup`] (a response: `text/html; charset=utf-8`) |
//! | Reusable views with children | [`Component`] + [`Slots`] → [`Slotted`] |
//! | Full page to browsers, fragment to htmx | [`Page`] + [`LayoutLayer`] / [`respond`], [`BasicLayout`] |
//! | htmx request and response headers | [`htmx::HxRequest`], [`htmx::HxTarget`], [`htmx::HxTrigger`], [`htmx::HxRedirect`], [`htmx::HxReswap`], [`htmx::HxRetarget`] |
//! | The htmx script, same origin | [`htmx::asset_route`], [`htmx::htmx_head`] |
//! | Forms that show their errors | [`form::HtmlForm`], [`form::FormState`] |
//! | Raw HTML (reviewed allowlist) | [`trusted_html`] |
//!
//! ## Secure by default (ADR 0013 R9)
//!
//! - maud escapes every interpolated value, in text and attributes. Raw HTML goes only through
//!   [`trusted_html`], and a test fails on any other `PreEscaped` in this crate or the admin
//!   crate.
//! - htmx is vendored ([`htmx::HTMX_JS`], sha384-pinned, 0BSD) and served by the app itself, so
//!   pages keep `Content-Security-Policy: script-src 'self'` (what
//!   [`SecurityLayer::recommended`](crate::SecurityLayer::recommended) sends). The layout turns
//!   off `eval` and script tags in swapped content and limits htmx to same-origin requests.
//! - [`BasicLayout`] sends the session's masked CSRF token on every htmx request
//!   (`hx-headers`); [`LayoutContext::csrf_field`] covers plain forms.
//! - [`htmx::HxRedirect`] only redirects within the origin.
//!
//! ## Using `html!`
//!
//! [`html!`] expands to paths starting with `maud::`, so bring the re-exported crate into scope
//! next to it: `use id_effect_axum::ui::{html, maud};` (or depend on `maud` directly, same
//! version).
//!
//! ```rust
//! use id_effect::schema::{self, HasSchema, Schema};
//! use id_effect::{Effect, succeed};
//! use id_effect_axum::route::{Input, Route, Router};
//! use id_effect_axum::ui::{Markup, html, maud};
//!
//! struct Name;
//!
//! impl HasSchema for Name {
//!   type A = String;
//!   type I = String;
//!   type E = ();
//!
//!   fn schema() -> Schema<String, String> {
//!     schema::string()
//!   }
//! }
//!
//! fn hello(_: &mut (), input: Input<String>) -> Effect<Markup, std::convert::Infallible, ()> {
//!   succeed(html! { p { "Hello, " (input.path) "!" } })
//! }
//!
//! let app: axum::Router = Router::new()
//!   .route(Route::get("/hello/{name}").public().path::<Name>().handler(hello))
//!   .with_state(())
//!   .into_axum();
//! # let _ = app;
//! let markup = html! { p { "Hello, " ("<script>") "!" } };
//! assert_eq!(markup.into_string(), "<p>Hello, &lt;script&gt;!</p>");
//! ```

pub mod component;
pub mod form;
pub mod htmx;
pub mod layout;
pub mod trusted_html;

pub use maud;
pub use maud::{DOCTYPE, Markup, Render, html};

pub use component::{Component, SlotContent, Slots, Slotted};
pub use form::{FieldError, FormState, HtmlForm, InputKind};
pub use htmx::{HxRedirect, HxRequest, HxReswap, HxRetarget, HxTarget, HxTrigger};
pub use layout::{BasicLayout, Layout, LayoutContext, LayoutLayer, LayoutService, Page, respond};
