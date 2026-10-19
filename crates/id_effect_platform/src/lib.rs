//! **Platform capabilities** for `id_effect` — HTTP, filesystem, and OS subprocess abstractions
//! inspired by Effect.ts [`@effect/platform`](https://effect.website/docs/platform/introduction).
//!
//! [`http`], [`fs`], [`command`], and [`uri`] are always available. Every effect is an
//! `id_effect::Effect`, and each capability ([`http::HttpClientService`],
//! [`fs::FileSystemService`], [`command::ProcessRuntimeService`]) is provided by a
//! `id_effect::Layer` (`ReqwestHttpClientProvider::layer()`, `LiveFileSystemProvider::layer()`,
//! `TokioProcessRuntimeProvider::layer()`, or the `provide_*` layers for a custom implementation).
//!
//! The live implementations (reqwest, `tokio::process`) need a tokio runtime with its IO driver
//! enabled: run them with `id_effect_tokio::TokioRuntime` (or `Runtime::run` inside such a runtime).
//!
//! ## Features
//!
//! - `auth` (off by default since 0.6) — session store and OAuth client capability traits
//!   (`auth` module). This is web-framework material and will move to a web crate.
//! - `test-util` (implies `auth`) — in-memory test doubles such as `MemoryOAuthClient`.
//! - `tls` — certificates, CA bundles, reload on rotation and rustls configs (`tls` module).
//! - `quic` (implies `tls`) — one QUIC endpoint with ALPN routing (`quic` module, ADR 0014 D6/D7).
//! - `http3` (implies `quic`), `acme` (implies `tls`) — HTTP/3 and ACME dependencies.
//!
//! ## Example (HTTP capability with a test double)
//!
//! Production code provides [`http::ReqwestHttpClientProvider::layer`]; tests swap the same
//! capability for a canned client.
//!
//! ```rust
//! use id_effect::Exit;
//! use id_effect::{Effect, fail, succeed};
//! use id_effect_platform::error::HttpError;
//! use id_effect_platform::http::{
//!   HttpClient, HttpClientEnv, HttpRequest, HttpResponse, StreamingHttpResponse, execute,
//!   provide_http_client,
//! };
//!
//! struct Canned;
//!
//! impl HttpClient for Canned {
//!   fn execute(&self, req: HttpRequest) -> Effect<HttpResponse, HttpError> {
//!     succeed(HttpResponse { status: 200, headers: vec![], body: req.url.into_bytes() })
//!   }
//!
//!   fn execute_stream(&self, _req: HttpRequest) -> Effect<StreamingHttpResponse, HttpError> {
//!     fail(HttpError::InvalidRequest("no streaming in this double".into()))
//!   }
//! }
//!
//! // `R` is named because a layer may provide more than the effect needs.
//! let program = execute::<HttpClientEnv, _>(HttpRequest::get("https://example.com/hi"))
//!   .provide_layer(provide_http_client(Canned));
//! let Exit::Success(res) = program.block_on() else { panic!("request failed") };
//! assert_eq!(res.status, 200);
//! assert_eq!(res.body, b"https://example.com/hi");
//! ```
//!
//! See the crate [`README.md`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect_platform/README.md)
//! and RFC [`0001-id-effect-platform.md`](../../docs/effect-ts-parity/rfcs/0001-id-effect-platform.md).

#![forbid(unsafe_code)]
#![deny(missing_docs)]

#[cfg(feature = "auth")]
pub mod auth;
pub mod command;
pub mod error;
pub mod fs;
pub mod http;
pub mod uri;

#[cfg(feature = "quic")]
pub mod quic;
#[cfg(feature = "tls")]
pub mod tls;

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
