//! Application server shell: config bootstrap, the server as a scoped layer, lifecycle, security
//! middleware, shutdown.

pub mod bootstrap;
pub mod lifecycle;
pub mod security;
pub mod shutdown;

pub use bootstrap::{HostConfig, bootstrap_env, load_host_config, provide_host_config_env};
pub use lifecycle::{
  Host, HostBuilder, HostError, ServerHandle, serve, serve_router, serve_until,
  serve_until_shutdown, server_layer, socket_addr,
};
pub use security::{
  CSRF_HEADER, ContentSecurityPolicy, CsrfConfig, csp_middleware, csrf_middleware, set_csrf_header,
};
pub use shutdown::{HostDrain, ShutdownReason, drain_with_timeout, wait_for_shutdown};
