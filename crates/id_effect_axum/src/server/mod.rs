//! Application server shell: config bootstrap, the server as a scoped layer, lifecycle, security
//! middleware, shutdown.

pub mod bootstrap;
pub mod lifecycle;
pub mod security;
pub mod shutdown;
pub mod static_files;

pub use bootstrap::{HostConfig, bootstrap_env, load_host_config, provide_host_config_env};
pub use lifecycle::{
  Host, HostBuilder, HostError, ServerHandle, serve, serve_router, serve_until,
  serve_until_shutdown, server_layer, socket_addr,
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
