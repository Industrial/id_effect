//! Application server shell: config bootstrap, the server as a scoped layer, lifecycle, security
//! middleware, shutdown.

pub mod alt_svc;
pub mod bootstrap;
pub mod connection;
#[cfg(feature = "http3")]
pub mod h3;
pub mod lifecycle;
pub mod security;
pub mod shutdown;
pub mod static_files;
#[cfg(feature = "tls")]
pub mod tls;
// r12-e12-3-webtransport: WebTransport sessions on the HTTP/3 adapter.
#[cfg(feature = "webtransport")]
pub mod webtransport;

pub use alt_svc::AltSvcLayer;
pub use bootstrap::{
  HTTP3_WITHOUT_TLS_WARNING, HostConfig, HostTls, bootstrap_env, load_host_config,
  provide_host_config_env,
};
#[cfg(feature = "tls")]
pub use connection::serve_tls_listener;
pub use connection::{
  ConnectionLimits, DANGEROUS_NO_HEADER_READ_TIMEOUT, DANGEROUS_NO_TLS_HANDSHAKE_TIMEOUT,
  DANGEROUS_UNLIMITED_CONCURRENT_STREAMS, DANGEROUS_UNLIMITED_CONNECTIONS,
  DANGEROUS_UNLIMITED_REQUESTS_PER_CONNECTION, HostTransport, serve_host_listener, serve_listener,
};
#[cfg(feature = "http3")]
pub use h3::{DANGEROUS_ALLOW_ZERO_RTT_FOR_QUERIES, Http3Options, quic_limits, serve_h3};
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
#[cfg(feature = "tls")]
pub use tls::{ServerTls, ServerTlsBuilder};
