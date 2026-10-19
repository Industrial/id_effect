//! Application server shell: config bootstrap, the server as a scoped layer, lifecycle, security
//! middleware, shutdown.

pub mod bootstrap;
pub mod connection;
pub mod lifecycle;
pub mod security;
pub mod shutdown;
pub mod static_files;

pub use bootstrap::{HostConfig, bootstrap_env, load_host_config, provide_host_config_env};
pub use connection::{ConnectionLimits, serve_listener};
pub use lifecycle::{
  Host, HostBuilder, HostError, ServerHandle, serve, serve_router, serve_router_with_limits,
  serve_until, serve_until_shutdown, serve_until_shutdown_with_limits, serve_with_limits,
  server_layer, server_layer_with_limits, socket_addr,
};
#[allow(deprecated)]
pub use security::{
  COOKIE_KEY_ENV, CSRF_COOKIE, CSRF_FORM_FIELD, CSRF_HEADER, ContentSecurityPolicy, CookieDefaults,
  CookieKey, Cookies, CorsConfig, CspNonce, CsrfConfig, CsrfToken, FrameOptions, SecurityError,
  SecurityHeaders, SecurityLayer, csp_middleware, csrf_middleware, set_csrf_header,
};
pub use shutdown::{
  HostDrain, ShutdownReason, drain_with_timeout, track_in_flight, wait_for_shutdown,
};
pub use static_files::{StaticFiles, serve_dir};
