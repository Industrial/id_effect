//! **TLS material** (feature `tls`): certificates, CA bundles, reload on rotation and the
//! rustls client and server configs that the encrypted transports share — the QUIC mesh and
//! TLS-TCP transports of `id_effect_node`, the `quic` endpoint (feature `quic`), and the
//! HTTP servers and clients built on them.
//!
//! Moved from `id_effect_node::transport::certs` in 0.12 (ADR 0014 D6); the node path is a
//! deprecated re-export for one minor.
//!
//! Every config is TLS 1.3 only and built with an explicit crypto provider ([`provider`]:
//! the process default, else aws-lc-rs). Trust is explicit: a dialer verifies servers by CA
//! roots and/or pinned SHA-256 fingerprints, and a listener verifies clients the same way —
//! [`ClientAuth::Required`] (handshake-level mutual TLS) or [`ClientAuth::Optional`] (the
//! shared endpoint of ADR 0014 D7, where the application refuses what needs a client).
//!
//! ```rust
//! use id_effect_platform::tls::{Identity, NodeCertificate, client_tls_config, server_tls_config};
//!
//! let server = Identity::from_certificate(&NodeCertificate::self_signed("node-b").unwrap()).unwrap();
//! let client = Identity::from_certificate(&NodeCertificate::self_signed("node-a").unwrap()).unwrap();
//! // Each side pins the other's leaf certificate.
//! let dial = client_tls_config(&client, None, &[server.cert_sha256()]).unwrap();
//! let listen = server_tls_config(&server, None, &[client.cert_sha256()]).unwrap();
//! assert!(dial.client_auth_cert_resolver.has_certs());
//! assert_eq!(listen.max_early_data_size, 0);
//! ```

#[cfg(feature = "acme")]
pub mod acme;
mod certs;
mod reload;

pub use certs::{
  CertError, CertReloader, ClientAuth, Identity, NodeCertificate, ReloadTask, ca_bundle_from_pem,
  client_cert_verifier, client_tls_config, load_ca_bundle, provider, server_tls_config,
  server_tls_config_with, sha256,
};
