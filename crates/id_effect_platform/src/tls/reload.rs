//! Swapping the certificate of a running server (feature `tls`; ADR 0014 D8, `dist.tls#7`).
//!
//! One [`Identity`] is the certificate slot every listener of a process reads on each TLS
//! handshake: hand clones of it to the QUIC endpoint (`quic::QuicEndpointConfig::new`), to the
//! TCP TLS listener (as its `rustls::server::ResolvesServerCert`) and to the mesh dialers. A
//! swap is then seen by the next handshake on every one of them, and by nothing already
//! established:
//!
//! - new connections, dialed or accepted, present the new certificate;
//! - connections already open keep the certificate they were established with — a TLS session
//!   never re-reads its certificate, so nothing is cut and nothing needs a restart.
//!
//! Three things swap the slot: [`CertReloader`](super::CertReloader) when the PEM files on
//! disk change, ACME when it has issued or renewed a certificate (feature `acme`,
//! `tls::acme::Acme`), and [`Identity::install`] by hand. Each swap checks that the key
//! belongs to the leaf first; a certificate that fails the check is refused and the current
//! one stays in use.
//!
//! ```rust
//! use id_effect_platform::tls::{Identity, NodeCertificate};
//!
//! let first = NodeCertificate::self_signed("svc.example").unwrap();
//! let identity = Identity::from_certificate(&first).unwrap();
//! let for_quic = identity.clone(); // the endpoint and the TCP listener hold clones
//!
//! let rotated = NodeCertificate::self_signed("svc.example").unwrap();
//! let installed = identity.install(&rotated).unwrap();
//! assert_eq!(installed, rotated.cert_sha256());
//! assert_eq!(for_quic.cert_sha256(), rotated.cert_sha256());
//! ```

use super::{CertError, Identity, NodeCertificate};

impl Identity {
  /// Present `cert` from the next handshake on, on every listener and dialer holding a clone
  /// of this identity; connections already open keep their certificate. Returns the
  /// SHA-256 fingerprint of the new leaf.
  ///
  /// Fails, leaving the current certificate in place, when the crypto provider cannot use the
  /// key.
  pub fn install(&self, cert: &NodeCertificate) -> Result<[u8; 32], CertError> {
    let key = cert.certified_key()?;
    let installed = cert.cert_sha256();
    self.replace(key);
    let sha256 = fingerprint_hex(&installed);
    tracing::info!(sha256, "certificate installed");
    Ok(installed)
  }
}

/// Lower-case hex of a fingerprint, for logs.
pub(crate) fn fingerprint_hex(fingerprint: &[u8; 32]) -> String {
  fingerprint.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn install_swaps_every_clone_and_returns_the_fingerprint() {
    let identity =
      Identity::from_certificate(&NodeCertificate::self_signed("a").expect("cert")).expect("id");
    let clone = identity.clone();
    let next = NodeCertificate::self_signed("b").expect("cert");
    assert_eq!(
      identity.install(&next).expect("install"),
      next.cert_sha256()
    );
    assert_eq!(clone.cert_sha256(), next.cert_sha256());
  }

  #[test]
  fn fingerprints_print_as_lower_case_hex() {
    let mut fingerprint = [0u8; 32];
    fingerprint[0] = 0xab;
    fingerprint[31] = 0x01;
    let hex = fingerprint_hex(&fingerprint);
    assert_eq!(hex.len(), 64);
    assert!(hex.starts_with("ab00") && hex.ends_with("0001"));
  }
}
