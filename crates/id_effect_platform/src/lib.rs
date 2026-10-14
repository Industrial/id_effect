//! **Platform capabilities** for `id_effect` — HTTP, filesystem, and OS subprocess abstractions
//! inspired by Effect.ts [`@effect/platform`](https://effect.website/docs/platform/introduction).
//!
//! [`http`], [`fs`], [`command`], and [`uri`] are always available.
//!
//! ## Features
//!
//! - `auth` (off by default since 0.6) — session store and OAuth client capability traits
//!   (`auth` module). This is web-framework material and will move to a web crate.
//! - `test-util` (implies `auth`) — in-memory test doubles such as `MemoryOAuthClient`.
//!
//! ## Example (HTTP capability with a test double)
//!
//! Production code registers [`http::provide_reqwest_http_client`] (e.g. with
//! [`id_effect::run_with`]); tests swap the same capability for a canned client.
//!
//! ```rust
//! use std::sync::Arc;
//!
//! use id_effect::{Effect, Env, fail, run_blocking, succeed};
//! use id_effect_platform::error::HttpError;
//! use id_effect_platform::http::{
//!   HttpClient, HttpRequest, HttpResponse, StreamingHttpResponse, env_set_http_client, execute,
//! };
//!
//! struct Canned;
//!
//! impl HttpClient for Canned {
//!   fn execute(&self, req: HttpRequest) -> Effect<HttpResponse, HttpError, ()> {
//!     succeed(HttpResponse { status: 200, headers: vec![], body: req.url.into_bytes() })
//!   }
//!
//!   fn execute_stream(&self, _req: HttpRequest) -> Effect<StreamingHttpResponse, HttpError, ()> {
//!     fail(HttpError::InvalidRequest("no streaming in this double".into()))
//!   }
//! }
//!
//! let mut env = Env::new();
//! env_set_http_client(&mut env, Arc::new(Canned));
//! let res = run_blocking(execute::<Env>(HttpRequest::get("https://example.com/hi")), env).unwrap();
//! assert_eq!(res.status, 200);
//! assert_eq!(res.body, b"https://example.com/hi");
//! ```
//!
//! See the crate [`README.md`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect_platform/README.md)
//! and RFC [`0001-id-effect-platform.md`](../../docs/effect-ts-parity/rfcs/0001-id-effect-platform.md).

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![allow(
  private_interfaces,
  private_bounds,
  clippy::new_ret_no_self,
  clippy::unused_unit
)]

#[cfg(feature = "auth")]
pub mod auth;
pub mod command;
pub mod error;
pub mod fs;
pub mod http;
pub mod uri;

/// Deprecated alias of [`command`] (OS subprocess spawning).
#[deprecated(since = "0.6.0", note = "renamed to `id_effect_platform::command`")]
pub mod process {
  pub use super::command::*;
}

#[cfg(feature = "test-util")]
pub use auth::MemoryOAuthClient;
#[cfg(feature = "auth")]
pub use auth::{
  MemorySessionStore, OAuthClient, OAuthError, OAuthTokens, OAuthUserInfo, SessionData,
  SessionError, SessionStore,
};

#[cfg(test)]
mod tests {
  use super::error::PlatformError;

  #[test]
  fn platform_error_unsupported_round_trips_display() {
    let e = PlatformError::Unsupported("sanity");
    assert!(e.to_string().contains("sanity"));
  }
}
