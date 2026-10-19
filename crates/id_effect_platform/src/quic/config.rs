//! Endpoint limits and the quinn configs built from them.

use std::sync::Arc;
use std::time::Duration;

use super::QuicError;
use crate::tls::Identity;

/// Posture id of [`QuicEndpointConfig::dangerous_no_retry`].
pub const DANGEROUS_NO_RETRY: &str = "id_effect_platform::QuicEndpointConfig::dangerous_no_retry";

/// Posture id of [`QuicEndpointConfig::dangerous_raise_limits`].
pub const DANGEROUS_RAISE_LIMITS: &str =
  "id_effect_platform::QuicEndpointConfig::dangerous_raise_limits";

/// The smallest UDP payload QUIC allows (RFC 9000 §14).
const MIN_UDP_PAYLOAD: u16 = 1200;

/// Finite resource limits of a QUIC endpoint (ADR 0013 R4). The defaults are the ceiling: a
/// value above one needs [`QuicEndpointConfig::dangerous_raise_limits`]; every value must be
/// non-zero.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct QuicLimits {
  /// Bidirectional streams a peer may have open at once on one connection (default 100).
  pub max_concurrent_bidi_streams: u32,
  /// Unidirectional streams a peer may have open at once (default 100; HTTP/3 needs 3).
  pub max_concurrent_uni_streams: u32,
  /// A connection with no traffic for this long is closed (default 30 s).
  pub idle_timeout: Duration,
  /// Largest UDP payload the endpoint receives (default 1472, at least 1200).
  pub max_udp_payload_size: u16,
  /// Bytes a peer may send on one stream ahead of the reader (default 1.25 MB).
  pub stream_receive_window: u32,
  /// Bytes a peer may send on one connection ahead of the reader (default 10 MB).
  pub receive_window: u32,
  /// Handshakes in progress at once; further connection attempts are refused (default 256).
  pub max_pending_handshakes: usize,
  /// A handshake, and a mesh peer opening its stream, must finish within this (default 10 s).
  pub handshake_timeout: Duration,
  /// Connections a route holds for its handler before refusing more (default 64).
  pub route_backlog: usize,
}

impl Default for QuicLimits {
  fn default() -> Self {
    Self {
      max_concurrent_bidi_streams: 100,
      max_concurrent_uni_streams: 100,
      idle_timeout: Duration::from_secs(30),
      max_udp_payload_size: 1472,
      stream_receive_window: 1_250_000,
      receive_window: 10_000_000,
      max_pending_handshakes: 256,
      handshake_timeout: Duration::from_secs(10),
      route_backlog: 64,
    }
  }
}

impl QuicLimits {
  /// The name of the first limit above `ceiling`.
  fn above(&self, ceiling: &Self) -> Option<&'static str> {
    [
      (
        "max_concurrent_bidi_streams",
        self.max_concurrent_bidi_streams > ceiling.max_concurrent_bidi_streams,
      ),
      (
        "max_concurrent_uni_streams",
        self.max_concurrent_uni_streams > ceiling.max_concurrent_uni_streams,
      ),
      ("idle_timeout", self.idle_timeout > ceiling.idle_timeout),
      (
        "max_udp_payload_size",
        self.max_udp_payload_size > ceiling.max_udp_payload_size,
      ),
      (
        "stream_receive_window",
        self.stream_receive_window > ceiling.stream_receive_window,
      ),
      (
        "receive_window",
        self.receive_window > ceiling.receive_window,
      ),
      (
        "max_pending_handshakes",
        self.max_pending_handshakes > ceiling.max_pending_handshakes,
      ),
      (
        "handshake_timeout",
        self.handshake_timeout > ceiling.handshake_timeout,
      ),
      ("route_backlog", self.route_backlog > ceiling.route_backlog),
    ]
    .into_iter()
    .find_map(|(name, above)| above.then_some(name))
  }

  /// Zero (or a UDP payload below QUIC's minimum) would disable what the limit bounds.
  fn check(&self) -> Result<(), QuicError> {
    let zero = [
      (
        "max_concurrent_bidi_streams",
        self.max_concurrent_bidi_streams == 0,
      ),
      ("idle_timeout", self.idle_timeout.is_zero()),
      ("stream_receive_window", self.stream_receive_window == 0),
      ("receive_window", self.receive_window == 0),
      ("max_pending_handshakes", self.max_pending_handshakes == 0),
      ("handshake_timeout", self.handshake_timeout.is_zero()),
      ("route_backlog", self.route_backlog == 0),
    ]
    .into_iter()
    .find_map(|(name, zero)| zero.then_some(name));
    if let Some(name) = zero {
      return Err(QuicError::Config(format!("limit {name} must not be zero")));
    }
    if self.max_udp_payload_size < MIN_UDP_PAYLOAD {
      return Err(QuicError::Config(format!(
        "limit max_udp_payload_size must be at least {MIN_UDP_PAYLOAD}"
      )));
    }
    Ok(())
  }
}

/// The quinn transport config for `limits`: stream credits, idle timeout and receive windows
/// bounded, datagrams off.
pub fn transport_config(limits: &QuicLimits) -> Result<quinn::TransportConfig, QuicError> {
  let idle = quinn::IdleTimeout::try_from(limits.idle_timeout)
    .map_err(|e| QuicError::Config(format!("idle_timeout: {e}")))?;
  let mut transport = quinn::TransportConfig::default();
  transport
    .max_concurrent_bidi_streams(quinn::VarInt::from_u32(limits.max_concurrent_bidi_streams))
    .max_concurrent_uni_streams(quinn::VarInt::from_u32(limits.max_concurrent_uni_streams))
    .max_idle_timeout(Some(idle))
    .stream_receive_window(quinn::VarInt::from_u32(limits.stream_receive_window))
    .receive_window(quinn::VarInt::from_u32(limits.receive_window))
    .datagram_receive_buffer_size(None);
  Ok(transport)
}

/// A quinn client config from a rustls one (for example
/// [`client_tls_config`](crate::tls::client_tls_config)) offering `alpns`, with `limits` and
/// without 0-RTT.
pub fn client_config(
  mut tls: rustls::ClientConfig,
  alpns: &[&[u8]],
  limits: &QuicLimits,
) -> Result<quinn::ClientConfig, QuicError> {
  tls.alpn_protocols = alpns.iter().map(|alpn| alpn.to_vec()).collect();
  tls.enable_early_data = false;
  let crypto = quinn::crypto::rustls::QuicClientConfig::try_from(tls)
    .map_err(|e| QuicError::Config(format!("tls: {e}")))?;
  let mut config = quinn::ClientConfig::new(Arc::new(crypto));
  config.transport_config(Arc::new(transport_config(limits)?));
  Ok(config)
}

/// What a [`QuicEndpoint`](super::QuicEndpoint) presents and how it protects itself.
#[derive(Clone, Debug)]
pub struct QuicEndpointConfig {
  identity: Identity,
  limits: QuicLimits,
  retry: bool,
  raised: bool,
}

impl QuicEndpointConfig {
  /// An endpoint presenting `identity` (for routes without their own) with the default limits
  /// and address validation on.
  pub fn new(identity: Identity) -> Self {
    Self {
      identity,
      limits: QuicLimits::default(),
      retry: true,
      raised: false,
    }
  }

  /// Use `limits`. Values above the defaults need [`Self::dangerous_raise_limits`]; checked
  /// when the endpoint binds.
  pub fn with_limits(mut self, limits: QuicLimits) -> Self {
    self.limits = limits;
    self
  }

  /// **Dangerous.** Accept connections without QUIC address validation (Retry). Recorded in
  /// the posture registry as [`DANGEROUS_NO_RETRY`].
  ///
  /// Without Retry the server spends a TLS handshake on every spoofed source address and can
  /// be used to amplify traffic towards a victim up to quinn's anti-amplification limit.
  pub fn dangerous_no_retry(mut self) -> Self {
    id_effect::observability::posture::record_dangerous(
      DANGEROUS_NO_RETRY,
      "R4/R8: the QUIC endpoint accepts connections without address validation (Retry), so \
       spoofed sources cost it handshakes",
    );
    self.retry = false;
    self
  }

  /// **Dangerous.** Allow [`QuicLimits`] above their defaults. Recorded in the posture
  /// registry as [`DANGEROUS_RAISE_LIMITS`].
  pub fn dangerous_raise_limits(mut self) -> Self {
    id_effect::observability::posture::record_dangerous(
      DANGEROUS_RAISE_LIMITS,
      "R4: QUIC stream, window, handshake or idle limits above the defaults let one peer hold \
       more of the endpoint",
    );
    self.raised = true;
    self
  }

  /// The identity presented by routes without their own.
  pub fn identity(&self) -> &Identity {
    &self.identity
  }

  /// The limits in force.
  pub fn limits(&self) -> &QuicLimits {
    &self.limits
  }

  /// Whether address validation (Retry) is on.
  pub fn retry(&self) -> bool {
    self.retry
  }

  /// Refuse zero limits, and limits above the defaults unless raised.
  pub(crate) fn validate(&self) -> Result<(), QuicError> {
    self.limits.check()?;
    match self.limits.above(&QuicLimits::default()) {
      Some(name) if !self.raised => Err(QuicError::LimitAboveDefault(name)),
      _ => Ok(()),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn every_limit_above_its_default_is_named() {
    let ceiling = QuicLimits::default();
    type Raise = fn(&mut QuicLimits);
    let raised: [(&str, Raise); 9] = [
      ("max_concurrent_bidi_streams", |l| {
        l.max_concurrent_bidi_streams += 1
      }),
      ("max_concurrent_uni_streams", |l| {
        l.max_concurrent_uni_streams += 1
      }),
      ("idle_timeout", |l| l.idle_timeout += Duration::from_secs(1)),
      ("max_udp_payload_size", |l| l.max_udp_payload_size += 1),
      ("stream_receive_window", |l| l.stream_receive_window += 1),
      ("receive_window", |l| l.receive_window += 1),
      ("max_pending_handshakes", |l| l.max_pending_handshakes += 1),
      ("handshake_timeout", |l| {
        l.handshake_timeout += Duration::from_secs(1)
      }),
      ("route_backlog", |l| l.route_backlog += 1),
    ];
    assert_eq!(ceiling.above(&ceiling), None);
    for (name, raise) in raised {
      let mut limits = QuicLimits::default();
      raise(&mut limits);
      assert_eq!(limits.above(&ceiling), Some(name));
    }
  }

  #[test]
  fn zero_limits_and_tiny_payloads_are_refused() {
    let limits = QuicLimits {
      route_backlog: 0,
      ..QuicLimits::default()
    };
    assert!(matches!(limits.check(), Err(QuicError::Config(m)) if m.contains("route_backlog")));
    let limits = QuicLimits {
      max_udp_payload_size: 1199,
      ..QuicLimits::default()
    };
    assert!(matches!(limits.check(), Err(QuicError::Config(m)) if m.contains("at least 1200")));
    // Tightening is free.
    let limits = QuicLimits {
      max_concurrent_bidi_streams: 1,
      max_concurrent_uni_streams: 0,
      ..QuicLimits::default()
    };
    assert_eq!(limits.check(), Ok(()));
  }

  #[test]
  fn transport_config_rejects_an_unrepresentable_idle_timeout() {
    let limits = QuicLimits {
      idle_timeout: Duration::MAX,
      ..QuicLimits::default()
    };
    assert!(matches!(
      transport_config(&limits),
      Err(QuicError::Config(m)) if m.starts_with("idle_timeout")
    ));
    assert!(matches!(
      client_config(
        crate::tls::client_tls_config(&identity(), None, &[]).expect("tls"),
        &[b"h3"],
        &limits
      ),
      Err(QuicError::Config(_))
    ));
  }

  fn identity() -> Identity {
    Identity::from_certificate(&crate::tls::NodeCertificate::self_signed("x").expect("cert"))
      .expect("identity")
  }

  #[test]
  fn raised_limits_need_the_opt_out() {
    let mut limits = QuicLimits::default();
    limits.receive_window *= 2;
    let config = QuicEndpointConfig::new(identity()).with_limits(limits);
    assert_eq!(
      config.validate(),
      Err(QuicError::LimitAboveDefault("receive_window"))
    );
    let config = config.dangerous_raise_limits();
    assert_eq!(config.validate(), Ok(()));
    assert!(
      id_effect::observability::posture::dangerous_opt_outs()
        .iter()
        .any(|(id, _)| *id == DANGEROUS_RAISE_LIMITS)
    );
  }
}
