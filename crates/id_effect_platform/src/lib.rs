//! **Platform capabilities** for `id_effect` — HTTP, filesystem, and process abstractions
//! inspired by Effect.ts [`@effect/platform`](https://effect.website/docs/platform/introduction).
//!
//! The crate ships **one** build: [`http`], [`fs`], [`process`], and [`uri`] are always available.
//!
//! ## Example (HTTP, capability DI v2)
//!
//! ```ignore
//! use id_effect::{RunError, build_env, provide, run_with};
//! use id_effect_platform::http::{HttpRequest, ReqwestHttpClientProvider, execute};
//!
//! # fn demo() -> Result<(), id_effect_platform::error::HttpError> {
//! let res = run_with(
//!   [provide!(ReqwestHttpClientProvider)],
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

pub mod error;
pub mod fs;
pub mod http;
pub mod process;
pub mod uri;

#[cfg(test)]
mod tests {
  use super::error::PlatformError;

  #[test]
  fn platform_error_unsupported_round_trips_display() {
    let e = PlatformError::Unsupported("sanity");
    assert!(e.to_string().contains("sanity"));
  }
}
