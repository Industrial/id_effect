//! **Deprecated since 0.12:** the certificate code moved to [`id_effect_platform::tls`]
//! (ADR 0014 D6), where the QUIC endpoint shared with HTTP/3 and the HTTP servers use it too.
//! This module re-exports it for one minor (L9); import from `id_effect_platform::tls`.
//!
//! - [`NodeCertificate`] — a node's chain and private key.
//! - [`load_ca_bundle`] / [`ca_bundle_from_pem`] — the CA roots peers are verified against.
//! - [`CertReloader`] — re-reads the chain and key files when they change.

pub use id_effect_platform::tls::{
  CertError, CertReloader, NodeCertificate, ReloadTask, ca_bundle_from_pem, load_ca_bundle,
};

#[cfg(feature = "tls-tcp")]
pub(crate) use super::tls_configs::{Identity, client_tls_config, server_tls_config};
