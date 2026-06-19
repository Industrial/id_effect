//! **Application host shell** for `id_effect` — lifecycle, config bootstrap, auth traits,
//! security middleware, and graceful shutdown.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![allow(clippy::new_ret_no_self, clippy::unused_unit)]

pub mod auth;
pub mod bootstrap;
pub mod lifecycle;
pub mod modules;
pub mod security;
pub mod shutdown;

pub use auth::{
  MemoryOAuthClient, MemorySessionStore, OAuthClient, OAuthError, OAuthTokens, OAuthUserInfo,
  SessionData, SessionError, SessionStore,
};
pub use bootstrap::{HostConfig, bootstrap_env, load_host_config, provide_host_config_env};
pub use lifecycle::{Host, HostBuilder, HostError};
pub use security::{
  CSRF_HEADER, ContentSecurityPolicy, CsrfConfig, csp_middleware, csrf_middleware, set_csrf_header,
};
pub use shutdown::{HostDrain, ShutdownReason, drain_with_timeout, wait_for_shutdown};
