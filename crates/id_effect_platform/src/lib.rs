//! **Platform capabilities** for `id_effect` — HTTP, filesystem, and OS subprocess abstractions
//! inspired by Effect.ts [`@effect/platform`](https://effect.website/docs/platform/introduction).
//!
//! The crate ships **one** build: [`http`], [`fs`], [`command`], and [`uri`] are always available.
//!
//! Enable the `test-util` feature for in-memory test doubles such as
//! `MemoryOAuthClient`.
//!
//! ## Example (HTTP, capability DI v2)
//!
//! ```ignore
//! use id_effect::{RunError, run_with};
//! use id_effect_platform::http::{HttpRequest, execute, provide_reqwest_http_client};
//!
//! # fn demo() -> Result<(), id_effect_platform::error::HttpError> {
//! let res = run_with(
//!   [provide_reqwest_http_client()],
//!   execute(HttpRequest::get("https://example.com")),
//! )
//! .map_err(|e| match e {
//!   RunError::Effect(e) => e,
//!   e => panic!("run failed: {e}"),
//! })?;
//! assert_eq!(res.status, 200);
//! # Ok(())
//! # }
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

#[cfg(any(test, feature = "test-util"))]
pub use auth::MemoryOAuthClient;
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
