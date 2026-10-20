//! Cookie-authenticated mutual handshake (wire protocol [`PROTOCOL_VERSION`], v3).
//!
//! BEAM's shared-secret cookie scheme with a modern MAC: both sides prove
//! knowledge of the cookie by HMAC-SHA256 over the whole handshake
//! transcript — both node identities, both 128-bit nonces, the direction of
//! the proof and, on transports with a secure channel, the channel's TLS
//! exporter secret ([`CHANNEL_BINDING_LABEL`], see
//! [`Connection::channel_binding`]). The cookie never travels on the wire.
//! Binding the proof to the TLS channel means a man-in-the-middle that
//! terminates TLS on both sides cannot relay one node's proof to another.
//!
//! This authenticates but does **not** encrypt — pair with a TLS transport
//! (QUIC, or TCP with feature `tls-tcp`) for confidentiality (ADR 0009
//! § security). On plaintext TCP (`TcpTransport::dangerous_plaintext`)
//! there is no channel to bind: the transcript stops a proof from being
//! reused across sessions, names or directions, but an on-path relay of the
//! raw bytes is not detectable there.
//!
//! Before the peer is authenticated the handshake bounds what it will
//! read: the whole exchange must finish within the cookie's
//! [`Cookie::handshake_timeout`] (default [`DEFAULT_HANDSHAKE_TIMEOUT`]),
//! and frames are capped at [`PRE_AUTH_MAX_FRAME_BYTES`]. Cookies shorter
//! than [`MIN_COOKIE_BYTES`] are refused as a configuration error.

use std::future::Future;
use std::time::Duration;

use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::id::NodeId;
use crate::transport::tcp::MAX_FRAME_BYTES;
use crate::transport::{Connection, TransportError};
use crate::wire::{Frame, PROTOCOL_VERSION, decode};

type HmacSha256 = Hmac<Sha256>;

/// Shortest cookie the handshake accepts, in bytes (256 bits of secret).
pub const MIN_COOKIE_BYTES: usize = 32;

/// Largest frame accepted from a peer before it has authenticated. The
/// connection's limit returns to [`MAX_FRAME_BYTES`] once the handshake
/// succeeds.
pub const PRE_AUTH_MAX_FRAME_BYTES: u32 = 4 * 1024;

/// Default bound on a whole handshake (see [`Cookie::with_handshake_timeout`]).
pub const DEFAULT_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// TLS exporter label (RFC 5705 / RFC 8446 § 7.5) of the 32-byte channel
/// binding mixed into the cookie proof on TLS transports.
pub const CHANNEL_BINDING_LABEL: &[u8] = b"EXPORTER-id-effect-mesh";

/// Posture id of [`Cookie::dangerous_allow_short`].
pub const DANGEROUS_ALLOW_SHORT_COOKIE: &str = "id_effect_node::Cookie::dangerous_allow_short";

/// Domain separator for the transcript MAC.
const TRANSCRIPT_DOMAIN: &[u8] = b"id-effect-mesh/handshake";

/// Shared cluster secret. Compared only through MACs, never sent raw.
///
/// The cookie also carries the handshake's timeout
/// ([`Cookie::with_handshake_timeout`]), since it is the one value every
/// handshake is given.
#[derive(Clone)]
pub struct Cookie {
  secret: Vec<u8>,
  allow_short: bool,
  handshake_timeout: Duration,
}

/// Why a cookie was refused as configuration.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum CookieError {
  /// The secret is shorter than [`MIN_COOKIE_BYTES`].
  #[error("cookie is {len} bytes; at least {min} are required")]
  TooShort {
    /// Length of the secret given.
    len: usize,
    /// The minimum, [`MIN_COOKIE_BYTES`].
    min: usize,
  },
}

impl Cookie {
  /// Cookie from secret bytes/string.
  ///
  /// Does not validate: a secret shorter than [`MIN_COOKIE_BYTES`] makes
  /// every handshake fail with a configuration error (unless
  /// [`Cookie::dangerous_allow_short`] is set). Prefer [`Cookie::try_new`],
  /// which reports that up front.
  pub fn new(secret: impl Into<Vec<u8>>) -> Self {
    Self {
      secret: secret.into(),
      allow_short: false,
      handshake_timeout: DEFAULT_HANDSHAKE_TIMEOUT,
    }
  }

  /// Cookie from secret bytes/string, refusing secrets shorter than
  /// [`MIN_COOKIE_BYTES`].
  pub fn try_new(secret: impl Into<Vec<u8>>) -> Result<Self, CookieError> {
    let cookie = Self::new(secret);
    cookie.check()?;
    Ok(cookie)
  }

  /// **Dangerous.** Accept a secret shorter than [`MIN_COOKIE_BYTES`].
  ///
  /// This re-opens offline guessing of the cookie: anyone who records one
  /// handshake can test candidate cookies against its MACs at hash speed,
  /// and a short or human-chosen secret falls quickly. Only for tests and
  /// throwaway clusters. Recorded in the posture registry as
  /// [`DANGEROUS_ALLOW_SHORT_COOKIE`].
  pub fn dangerous_allow_short(mut self) -> Self {
    id_effect::observability::posture::record_dangerous(
      DANGEROUS_ALLOW_SHORT_COOKIE,
      "M9: the cluster cookie may be shorter than MIN_COOKIE_BYTES, so one recorded handshake \
       lets an attacker guess it offline",
    );
    self.allow_short = true;
    self
  }

  /// Bound the whole handshake (default [`DEFAULT_HANDSHAKE_TIMEOUT`]). A
  /// peer that has not completed it in time is dropped, so a silent or
  /// slow connection cannot hold a handshake open.
  pub fn with_handshake_timeout(mut self, timeout: Duration) -> Self {
    self.handshake_timeout = timeout;
    self
  }

  /// The bound on a whole handshake.
  pub fn handshake_timeout(&self) -> Duration {
    self.handshake_timeout
  }

  fn check(&self) -> Result<(), CookieError> {
    if self.secret.len() < MIN_COOKIE_BYTES && !self.allow_short {
      return Err(CookieError::TooShort {
        len: self.secret.len(),
        min: MIN_COOKIE_BYTES,
      });
    }
    Ok(())
  }

  fn mac(&self, role: Role, transcript: &Transcript<'_>) -> HmacSha256 {
    let mut mac = HmacSha256::new_from_slice(&self.secret).expect("HMAC accepts any key length");
    mac.update(TRANSCRIPT_DOMAIN);
    mac.update(&PROTOCOL_VERSION.to_be_bytes());
    mac.update(&[role as u8]);
    for node in [transcript.initiator, transcript.responder] {
      mac.update(&(node.name().len() as u64).to_be_bytes());
      mac.update(node.name().as_bytes());
      mac.update(&node.incarnation().to_be_bytes());
    }
    mac.update(&transcript.initiator_nonce.to_be_bytes());
    mac.update(&transcript.responder_nonce.to_be_bytes());
    match &transcript.binding {
      Some(binding) => {
        mac.update(&[1]);
        mac.update(binding);
      }
      None => mac.update(&[0]),
    }
    mac
  }

  fn digest(&self, role: Role, transcript: &Transcript<'_>) -> [u8; 32] {
    self.mac(role, transcript).finalize().into_bytes().into()
  }

  fn verify(&self, role: Role, transcript: &Transcript<'_>, digest: &[u8; 32]) -> bool {
    self.mac(role, transcript).verify_slice(digest).is_ok()
  }
}

/// Which side a proof comes from; MACed so a proof cannot be reflected.
#[derive(Clone, Copy, Debug)]
enum Role {
  Initiator = 1,
  Responder = 2,
}

/// Everything a proof commits to.
struct Transcript<'a> {
  initiator: &'a NodeId,
  responder: &'a NodeId,
  initiator_nonce: u128,
  responder_nonce: u128,
  binding: Option<[u8; 32]>,
}

/// Why a handshake failed.
#[derive(Debug, thiserror::Error)]
pub enum HandshakeError {
  /// Transport died mid-handshake.
  #[error("transport: {0}")]
  Transport(#[from] TransportError),
  /// Peer sent something that is not a valid handshake frame, the
  /// handshake timed out, or the local cookie is misconfigured.
  #[error("protocol violation: {0}")]
  Protocol(String),
  /// Peer speaks a different protocol version.
  #[error("version mismatch: ours {ours}, theirs {theirs}")]
  VersionMismatch {
    /// Our protocol version.
    ours: u16,
    /// Peer's protocol version.
    theirs: u16,
  },
  /// Cookie proof failed — not part of this cluster.
  #[error("bad cookie")]
  BadCookie,
  /// Peer explicitly refused.
  #[error("rejected by peer: {0}")]
  Rejected(String),
}

/// Handshake codec. Mirrors `Frame::Handshake(HandshakeMsg)` variant for
/// variant, so `Hello` and `Rejected` stay byte-compatible with protocol v1
/// (the version check and its refusal must reach an older peer intact);
/// only the nonces widen to 128 bits, which [`crate::wire::HandshakeMsg`]
/// cannot carry without a breaking change.
#[derive(Debug, Serialize, Deserialize)]
enum HsFrame {
  Handshake(Hs),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
enum Hs {
  Hello {
    version: u16,
    node: NodeId,
  },
  Challenge {
    version: u16,
    node: NodeId,
    nonce: u128,
  },
  ChallengeReply {
    digest: [u8; 32],
    nonce: u128,
  },
  Welcome {
    digest: [u8; 32],
  },
  Rejected {
    reason: String,
  },
}

async fn send_hs(conn: &dyn Connection, msg: Hs) -> Result<(), HandshakeError> {
  let bytes = postcard::to_allocvec(&HsFrame::Handshake(msg))
    .map_err(|e| HandshakeError::Protocol(format!("encode: {e}")))?;
  conn.send(bytes.into()).await?;
  Ok(())
}

async fn recv_hs(conn: &dyn Connection) -> Result<Hs, HandshakeError> {
  let bytes = conn.recv().await?;
  match postcard::from_bytes::<HsFrame>(&bytes) {
    Ok(HsFrame::Handshake(msg)) => Ok(msg),
    Err(e) => match decode(&bytes) {
      Ok(Frame::Handshake(_)) | Err(_) => Err(HandshakeError::Protocol(format!("decode: {e}"))),
      Ok(other) => Err(HandshakeError::Protocol(format!(
        "expected handshake frame, got {other:?}"
      ))),
    },
  }
}

/// The version a refusal names, when it is a responder's version refusal.
fn refused_version(reason: &str) -> Option<u16> {
  let (have, _) = reason
    .strip_prefix("version mismatch: have ")?
    .split_once(", got ")?;
  have.parse().ok()
}

/// Shared guard rails: cookie check, pre-auth frame cap, timeout.
async fn guarded(
  conn: &dyn Connection,
  cookie: &Cookie,
  run: impl Future<Output = Result<NodeId, HandshakeError>>,
) -> Result<NodeId, HandshakeError> {
  cookie
    .check()
    .map_err(|e| HandshakeError::Protocol(format!("config: {e}")))?;
  conn.set_max_frame_bytes(PRE_AUTH_MAX_FRAME_BYTES);
  let timeout = cookie.handshake_timeout;
  let peer = tokio::time::timeout(timeout, run)
    .await
    .map_err(|_| HandshakeError::Protocol(format!("handshake timed out after {timeout:?}")))??;
  conn.set_max_frame_bytes(MAX_FRAME_BYTES);
  Ok(peer)
}

/// Run the initiator side. On success the connection is authenticated and
/// the peer's [`NodeId`] is returned.
pub async fn initiate(
  conn: &dyn Connection,
  me: &NodeId,
  cookie: &Cookie,
) -> Result<NodeId, HandshakeError> {
  guarded(conn, cookie, run_initiator(conn, me, cookie)).await
}

async fn run_initiator(
  conn: &dyn Connection,
  me: &NodeId,
  cookie: &Cookie,
) -> Result<NodeId, HandshakeError> {
  send_hs(
    conn,
    Hs::Hello {
      version: PROTOCOL_VERSION,
      node: me.clone(),
    },
  )
  .await?;

  let (peer, their_nonce) = match recv_hs(conn).await? {
    Hs::Challenge {
      version,
      node,
      nonce,
    } => {
      if version != PROTOCOL_VERSION {
        return Err(HandshakeError::VersionMismatch {
          ours: PROTOCOL_VERSION,
          theirs: version,
        });
      }
      (node, nonce)
    }
    Hs::Rejected { reason } => {
      return Err(match refused_version(&reason) {
        Some(theirs) => HandshakeError::VersionMismatch {
          ours: PROTOCOL_VERSION,
          theirs,
        },
        None => HandshakeError::Rejected(reason),
      });
    }
    other => {
      return Err(HandshakeError::Protocol(format!(
        "expected Challenge, got {other:?}"
      )));
    }
  };

  let my_nonce: u128 = rand::random();
  let transcript = Transcript {
    initiator: me,
    responder: &peer,
    initiator_nonce: my_nonce,
    responder_nonce: their_nonce,
    binding: conn.channel_binding(),
  };
  send_hs(
    conn,
    Hs::ChallengeReply {
      digest: cookie.digest(Role::Initiator, &transcript),
      nonce: my_nonce,
    },
  )
  .await?;

  match recv_hs(conn).await? {
    Hs::Welcome { digest } => {
      if cookie.verify(Role::Responder, &transcript, &digest) {
        Ok(peer)
      } else {
        Err(HandshakeError::BadCookie)
      }
    }
    Hs::Rejected { reason } => Err(HandshakeError::Rejected(reason)),
    other => Err(HandshakeError::Protocol(format!(
      "expected Welcome, got {other:?}"
    ))),
  }
}

/// Run the responder side. On success the connection is authenticated and
/// the peer's [`NodeId`] is returned.
pub async fn respond(
  conn: &dyn Connection,
  me: &NodeId,
  cookie: &Cookie,
) -> Result<NodeId, HandshakeError> {
  guarded(conn, cookie, run_responder(conn, me, cookie)).await
}

async fn run_responder(
  conn: &dyn Connection,
  me: &NodeId,
  cookie: &Cookie,
) -> Result<NodeId, HandshakeError> {
  let peer = match recv_hs(conn).await? {
    Hs::Hello { version, node } => {
      if version != PROTOCOL_VERSION {
        let _ = send_hs(
          conn,
          Hs::Rejected {
            reason: format!("version mismatch: have {PROTOCOL_VERSION}, got {version}"),
          },
        )
        .await;
        return Err(HandshakeError::VersionMismatch {
          ours: PROTOCOL_VERSION,
          theirs: version,
        });
      }
      node
    }
    other => {
      return Err(HandshakeError::Protocol(format!(
        "expected Hello, got {other:?}"
      )));
    }
  };

  let my_nonce: u128 = rand::random();
  send_hs(
    conn,
    Hs::Challenge {
      version: PROTOCOL_VERSION,
      node: me.clone(),
      nonce: my_nonce,
    },
  )
  .await?;

  match recv_hs(conn).await? {
    Hs::ChallengeReply { digest, nonce } => {
      let transcript = Transcript {
        initiator: &peer,
        responder: me,
        initiator_nonce: nonce,
        responder_nonce: my_nonce,
        binding: conn.channel_binding(),
      };
      if !cookie.verify(Role::Initiator, &transcript, &digest) {
        let _ = send_hs(
          conn,
          Hs::Rejected {
            reason: "bad cookie".to_string(),
          },
        )
        .await;
        return Err(HandshakeError::BadCookie);
      }
      send_hs(
        conn,
        Hs::Welcome {
          digest: cookie.digest(Role::Responder, &transcript),
        },
      )
      .await?;
      Ok(peer)
    }
    Hs::Rejected { reason } => Err(HandshakeError::Rejected(reason)),
    other => Err(HandshakeError::Protocol(format!(
      "expected ChallengeReply, got {other:?}"
    ))),
  }
}

#[cfg(test)]
mod tests {
  use bytes::Bytes;

  use super::*;
  use crate::transport::Transport;
  use crate::transport::memory::MemoryNetwork;
  use crate::wire::{HandshakeMsg, encode};

  const SECRET: &[u8; 32] = b"0123456789abcdef0123456789abcdef";

  /// Connected pair: (side under test, raw scripted peer).
  async fn pair() -> (Box<dyn Connection>, Box<dyn Connection>) {
    let net = MemoryNetwork::new();
    let listener = net.transport("b").listen("b").await.expect("bind");
    let dial = net.transport("a").connect("b").await.expect("dial");
    let accepted = listener.accept().await.expect("accept");
    (dial, accepted)
  }

  fn me() -> NodeId {
    NodeId::from_parts("a@test", 1)
  }

  fn peer() -> NodeId {
    NodeId::from_parts("b@test", 2)
  }

  fn cookie() -> Cookie {
    Cookie::new(SECRET.to_vec())
  }

  fn transcript<'a>(initiator: &'a NodeId, responder: &'a NodeId) -> Transcript<'a> {
    Transcript {
      initiator,
      responder,
      initiator_nonce: 9,
      responder_nonce: 10,
      binding: None,
    }
  }

  async fn run_initiate(conn: &dyn Connection) -> Result<NodeId, HandshakeError> {
    initiate(conn, &me(), &cookie()).await
  }

  async fn run_respond(conn: &dyn Connection) -> Result<NodeId, HandshakeError> {
    respond(conn, &me(), &cookie()).await
  }

  async fn send_raw(conn: &dyn Connection, msg: Hs) {
    send_hs(conn, msg).await.expect("raw send");
  }

  #[test]
  fn digest_verifies_under_the_same_cookie() {
    let (a, b) = (me(), peer());
    let t = transcript(&a, &b);
    let digest = cookie().digest(Role::Initiator, &t);
    assert!(cookie().verify(Role::Initiator, &t, &digest));
  }

  #[test]
  fn digest_fails_under_a_different_cookie() {
    let (a, b) = (me(), peer());
    let t = transcript(&a, &b);
    let digest = cookie().digest(Role::Initiator, &t);
    let other = Cookie::new(b"another-cluster-secret-of-32-byt".to_vec());
    assert!(!other.verify(Role::Initiator, &t, &digest));
  }

  #[test]
  fn digest_is_bound_to_the_whole_transcript() {
    let (a, b, c) = (me(), peer(), NodeId::from_parts("c@test", 3));
    let t = transcript(&a, &b);
    let digest = cookie().digest(Role::Initiator, &t);
    let check = |t: &Transcript<'_>, role, why: &str| {
      assert!(!cookie().verify(role, t, &digest), "{why}");
    };
    check(&t, Role::Responder, "reflected proof");
    check(&transcript(&a, &c), Role::Initiator, "other responder");
    check(&transcript(&c, &b), Role::Initiator, "other initiator");
    check(&transcript(&b, &a), Role::Initiator, "swapped direction");
    let mut moved = transcript(&a, &b);
    moved.initiator_nonce = 11;
    check(&moved, Role::Initiator, "other initiator nonce");
    let mut moved = transcript(&a, &b);
    moved.responder_nonce = 11;
    check(&moved, Role::Initiator, "replayed responder nonce");
    let mut bound = transcript(&a, &b);
    bound.binding = Some([0; 32]);
    check(&bound, Role::Initiator, "proof made off-channel");
    let incarnation = NodeId::from_parts("a@test", 99);
    check(
      &transcript(&incarnation, &b),
      Role::Initiator,
      "other incarnation",
    );
  }

  #[test]
  fn try_new_enforces_the_minimum_length() {
    assert_eq!(
      Cookie::try_new("short").err(),
      Some(CookieError::TooShort {
        len: 5,
        min: MIN_COOKIE_BYTES
      })
    );
    assert!(Cookie::try_new(SECRET.to_vec()).is_ok());
    assert_eq!(
      CookieError::TooShort { len: 5, min: 32 }.to_string(),
      "cookie is 5 bytes; at least 32 are required"
    );
  }

  #[test]
  fn dangerous_allow_short_still_macs() {
    let short = Cookie::new(Vec::<u8>::new()).dangerous_allow_short();
    assert!(short.check().is_ok());
    let (a, b) = (me(), peer());
    let t = transcript(&a, &b);
    let digest = short.digest(Role::Responder, &t);
    assert!(short.verify(Role::Responder, &t, &digest));
  }

  #[test]
  fn dangerous_allow_short_is_recorded_in_posture() {
    let _ = Cookie::new("short").dangerous_allow_short();
    assert!(
      id_effect::observability::posture::dangerous_opt_outs()
        .iter()
        .any(|(id, reopens)| *id == DANGEROUS_ALLOW_SHORT_COOKIE && reopens.contains("M9"))
    );
  }

  #[test]
  fn handshake_timeout_defaults_and_is_configurable() {
    assert_eq!(cookie().handshake_timeout(), DEFAULT_HANDSHAKE_TIMEOUT);
    assert_eq!(DEFAULT_HANDSHAKE_TIMEOUT, Duration::from_secs(5));
    let custom = cookie().with_handshake_timeout(Duration::from_millis(250));
    assert_eq!(custom.handshake_timeout(), Duration::from_millis(250));
  }

  #[test]
  fn refused_version_parses_only_version_refusals() {
    assert_eq!(refused_version("version mismatch: have 1, got 2"), Some(1));
    assert_eq!(refused_version("bad cookie"), None);
    assert_eq!(refused_version("version mismatch: have x, got 2"), None);
  }

  #[test]
  fn codec_mirrors_the_public_v1_frames() {
    // Hello and Rejected must reach (and come back from) older peers intact.
    let hello = Hs::Hello {
      version: 1,
      node: me(),
    };
    let public = encode(&Frame::Handshake(HandshakeMsg::Hello {
      version: 1,
      node: me(),
    }))
    .expect("encode");
    assert_eq!(
      postcard::to_allocvec(&HsFrame::Handshake(hello)).expect("encode"),
      public.to_vec()
    );
    let rejected = encode(&Frame::Handshake(HandshakeMsg::Rejected {
      reason: "version mismatch: have 1, got 2".into(),
    }))
    .expect("encode");
    assert!(matches!(
      postcard::from_bytes::<HsFrame>(&rejected).expect("decode"),
      HsFrame::Handshake(Hs::Rejected { .. })
    ));
  }

  #[test]
  fn error_display_is_human_readable() {
    assert_eq!(HandshakeError::BadCookie.to_string(), "bad cookie");
    assert_eq!(
      HandshakeError::VersionMismatch { ours: 1, theirs: 2 }.to_string(),
      "version mismatch: ours 1, theirs 2"
    );
    assert_eq!(
      HandshakeError::Rejected("nope".into()).to_string(),
      "rejected by peer: nope"
    );
    assert_eq!(
      HandshakeError::Protocol("x".into()).to_string(),
      "protocol violation: x"
    );
    let from: HandshakeError = TransportError::Closed.into();
    assert_eq!(from.to_string(), "transport: connection closed");
  }

  #[tokio::test]
  async fn responder_rejects_version_skew_and_tells_the_peer() {
    let (raw, conn) = pair().await;
    send_raw(
      raw.as_ref(),
      Hs::Hello {
        version: PROTOCOL_VERSION + 1,
        node: peer(),
      },
    )
    .await;
    let err = run_respond(conn.as_ref()).await.expect_err("skew");
    assert!(matches!(
      err,
      HandshakeError::VersionMismatch { ours: PROTOCOL_VERSION, theirs } if theirs == PROTOCOL_VERSION + 1
    ));
    assert!(matches!(
      recv_hs(raw.as_ref()).await.expect("rejection"),
      Hs::Rejected { .. }
    ));
  }

  #[tokio::test]
  async fn responder_refuses_a_non_hello_opening() {
    let (raw, conn) = pair().await;
    send_raw(raw.as_ref(), Hs::Welcome { digest: [0; 32] }).await;
    let err = run_respond(conn.as_ref()).await.expect_err("protocol");
    assert!(matches!(err, HandshakeError::Protocol(_)), "{err}");
  }

  #[tokio::test]
  async fn responder_refuses_a_data_frame_before_auth() {
    let (raw, conn) = pair().await;
    raw
      .send(encode(&Frame::Data(vec![1])).expect("encode"))
      .await
      .expect("send");
    let err = run_respond(conn.as_ref()).await.expect_err("protocol");
    assert!(
      matches!(err, HandshakeError::Protocol(msg) if msg.contains("expected handshake frame"))
    );
  }

  #[tokio::test]
  async fn garbage_bytes_are_a_protocol_violation() {
    let (raw, conn) = pair().await;
    raw
      .send(Bytes::from_static(&[0xff, 0xff, 0xff]))
      .await
      .expect("send");
    let err = run_respond(conn.as_ref()).await.expect_err("protocol");
    assert!(matches!(err, HandshakeError::Protocol(msg) if msg.starts_with("decode")));
  }

  #[tokio::test]
  async fn responder_rejects_a_forged_challenge_reply() {
    let (raw, conn) = pair().await;
    let script = async {
      send_raw(
        raw.as_ref(),
        Hs::Hello {
          version: PROTOCOL_VERSION,
          node: peer(),
        },
      )
      .await;
      assert!(matches!(
        recv_hs(raw.as_ref()).await.expect("challenge"),
        Hs::Challenge { .. }
      ));
      send_raw(
        raw.as_ref(),
        Hs::ChallengeReply {
          digest: [0; 32],
          nonce: 1,
        },
      )
      .await;
      recv_hs(raw.as_ref()).await.expect("verdict")
    };
    let (verdict, result) = tokio::join!(script, run_respond(conn.as_ref()));
    assert!(matches!(result, Err(HandshakeError::BadCookie)));
    assert!(matches!(verdict, Hs::Rejected { reason } if reason == "bad cookie"));
  }

  #[tokio::test]
  async fn responder_surfaces_an_initiator_rejection() {
    let (raw, conn) = pair().await;
    let script = async {
      send_raw(
        raw.as_ref(),
        Hs::Hello {
          version: PROTOCOL_VERSION,
          node: peer(),
        },
      )
      .await;
      let _ = recv_hs(raw.as_ref()).await.expect("challenge");
      send_raw(
        raw.as_ref(),
        Hs::Rejected {
          reason: "changed my mind".into(),
        },
      )
      .await;
    };
    let ((), result) = tokio::join!(script, run_respond(conn.as_ref()));
    assert!(matches!(result, Err(HandshakeError::Rejected(r)) if r == "changed my mind"));
  }

  #[tokio::test]
  async fn initiator_surfaces_an_explicit_rejection() {
    let (conn, raw) = pair().await;
    let script = async {
      let _ = recv_hs(raw.as_ref()).await.expect("hello");
      send_raw(
        raw.as_ref(),
        Hs::Rejected {
          reason: "cluster full".into(),
        },
      )
      .await;
    };
    let ((), result) = tokio::join!(script, run_initiate(conn.as_ref()));
    assert!(matches!(result, Err(HandshakeError::Rejected(r)) if r == "cluster full"));
  }

  /// A 0.10.0 responder refuses our Hello with its v1 wording; that is a
  /// version mismatch, not an opaque rejection.
  #[tokio::test]
  async fn initiator_reads_an_older_peers_refusal_as_version_mismatch() {
    let (conn, raw) = pair().await;
    let script = async {
      let _ = recv_hs(raw.as_ref()).await.expect("hello");
      raw
        .send(
          encode(&Frame::Handshake(HandshakeMsg::Rejected {
            reason: format!("version mismatch: have 1, got {PROTOCOL_VERSION}"),
          }))
          .expect("encode"),
        )
        .await
        .expect("send");
    };
    let ((), result) = tokio::join!(script, run_initiate(conn.as_ref()));
    assert!(matches!(
      result,
      Err(HandshakeError::VersionMismatch {
        ours: PROTOCOL_VERSION,
        theirs: 1
      })
    ));
  }

  #[tokio::test]
  async fn initiator_rejects_version_skew_in_challenge() {
    let (conn, raw) = pair().await;
    let script = async {
      let _ = recv_hs(raw.as_ref()).await.expect("hello");
      send_raw(
        raw.as_ref(),
        Hs::Challenge {
          version: 99,
          node: peer(),
          nonce: 5,
        },
      )
      .await;
    };
    let ((), result) = tokio::join!(script, run_initiate(conn.as_ref()));
    assert!(matches!(
      result,
      Err(HandshakeError::VersionMismatch { theirs: 99, .. })
    ));
  }

  #[tokio::test]
  async fn initiator_refuses_an_unexpected_reply_to_hello() {
    let (conn, raw) = pair().await;
    let script = async {
      let _ = recv_hs(raw.as_ref()).await.expect("hello");
      send_raw(raw.as_ref(), Hs::Welcome { digest: [0; 32] }).await;
    };
    let ((), result) = tokio::join!(script, run_initiate(conn.as_ref()));
    assert!(matches!(result, Err(HandshakeError::Protocol(m)) if m.contains("expected Challenge")));
  }

  #[tokio::test]
  async fn initiator_detects_a_responder_without_the_cookie() {
    let (conn, raw) = pair().await;
    let script = async {
      let _ = recv_hs(raw.as_ref()).await.expect("hello");
      send_raw(
        raw.as_ref(),
        Hs::Challenge {
          version: PROTOCOL_VERSION,
          node: peer(),
          nonce: 5,
        },
      )
      .await;
      let _ = recv_hs(raw.as_ref()).await.expect("reply");
      // An impostor cannot MAC the transcript without the cookie.
      send_raw(raw.as_ref(), Hs::Welcome { digest: [0; 32] }).await;
    };
    let ((), result) = tokio::join!(script, run_initiate(conn.as_ref()));
    assert!(matches!(result, Err(HandshakeError::BadCookie)));
  }

  #[tokio::test]
  async fn initiator_sends_a_proof_bound_to_the_transcript() {
    let (conn, raw) = pair().await;
    let cookie = cookie();
    let (a, b) = (me(), peer());
    let script = async {
      let hello = recv_hs(raw.as_ref()).await.expect("hello");
      assert!(matches!(hello, Hs::Hello { version: PROTOCOL_VERSION, ref node } if *node == me()));
      send_raw(
        raw.as_ref(),
        Hs::Challenge {
          version: PROTOCOL_VERSION,
          node: peer(),
          nonce: 77,
        },
      )
      .await;
      let Hs::ChallengeReply { digest, nonce } = recv_hs(raw.as_ref()).await.expect("reply") else {
        panic!("expected ChallengeReply");
      };
      let t = Transcript {
        initiator: &a,
        responder: &b,
        initiator_nonce: nonce,
        responder_nonce: 77,
        binding: None,
      };
      assert!(cookie.verify(Role::Initiator, &t, &digest));
      send_raw(
        raw.as_ref(),
        Hs::Welcome {
          digest: cookie.digest(Role::Responder, &t),
        },
      )
      .await;
    };
    let ((), result) = tokio::join!(script, initiate(conn.as_ref(), &a, &cookie));
    assert_eq!(result.expect("authenticated"), peer());
  }

  /// M9: nonces are 128-bit (a `u64` nonce has a 2^-64 chance of exceeding
  /// `u64::MAX`; a uniform `u128` almost always does).
  #[tokio::test]
  async fn m9_nonces_are_128_bit() {
    let (raw, conn) = pair().await;
    let script = async {
      send_raw(
        raw.as_ref(),
        Hs::Hello {
          version: PROTOCOL_VERSION,
          node: peer(),
        },
      )
      .await;
      let Hs::Challenge { nonce, .. } = recv_hs(raw.as_ref()).await.expect("challenge") else {
        panic!("expected Challenge");
      };
      raw.close().await;
      nonce
    };
    let (nonce, _) = tokio::join!(script, run_respond(conn.as_ref()));
    assert!(nonce > u128::from(u64::MAX), "nonce {nonce:#x}");
  }

  #[tokio::test]
  async fn short_cookie_fails_before_anything_is_sent() {
    let (conn, raw) = pair().await;
    let err = initiate(conn.as_ref(), &me(), &Cookie::new("short"))
      .await
      .expect_err("config");
    assert!(
      matches!(err, HandshakeError::Protocol(m) if m.starts_with("config: cookie is 5 bytes"))
    );
    conn.close().await;
    assert!(matches!(raw.recv().await, Err(TransportError::Closed)));
  }

  #[tokio::test]
  async fn closed_transport_is_a_transport_error() {
    let (conn, raw) = pair().await;
    raw.close().await;
    let err = run_initiate(conn.as_ref()).await.expect_err("closed");
    assert!(matches!(
      err,
      HandshakeError::Transport(TransportError::Closed)
    ));
  }
}
