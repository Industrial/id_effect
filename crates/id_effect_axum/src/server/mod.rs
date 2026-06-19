//! Application server shell: config bootstrap, lifecycle, security middleware, shutdown.

pub mod bootstrap;
pub mod lifecycle;
pub mod security;
pub mod shutdown;

pub use bootstrap::{HostConfig, bootstrap_env, load_host_config, provide_host_config_env};
pub use lifecycle::{Host, HostBuilder, HostError, serve_router, socket_addr};
pub use security::{
  CSRF_HEADER, ContentSecurityPolicy, CsrfConfig, csp_middleware, csrf_middleware, set_csrf_header,
};
pub use shutdown::{HostDrain, ShutdownReason, drain_with_timeout, wait_for_shutdown};
