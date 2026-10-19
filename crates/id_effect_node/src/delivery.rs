//! At-least-once delivery of remote casts and bare messages (0.11.0 D19, wire protocol v3).
//!
//! The mesh default stays **at-most-once** (ADR 0009, OTP semantics): a message in flight when
//! the connection drops is lost. At-least-once is opt-in, per send with [`Mesh::send_reliable`]
//! and [`Mesh::cast_reliable`] (or [`SendReliable::send_reliable`] on an [`Addr`]), or per mesh
//! with [`DeliveryMode::AtLeastOnce`] ([`Mesh::with_delivery`]), which makes every remote cast
//! and send reliable. Calls, spawns, monitors and links keep their own semantics.
//!
//! - **Sender.** Each peer gets sequence numbers from 1. A reliable message stays in the peer's
//!   bounded retransmit buffer ([`DeliveryConfig::max_unacked`] messages and
//!   [`DeliveryConfig::max_unacked_bytes`] payload bytes) until the peer acknowledges it. Unacked
//!   messages are sent again when the peer reconnects ([`Mesh::add_peer`],
//!   [`Mesh::add_peer_node`]) and on a timer with exponential backoff
//!   ([`DeliveryConfig::with_backoff`], reset by every ack that makes progress). A failed
//!   transmission keeps the message buffered; [`Mesh::peer_down`] does not drop it.
//! - **Bound.** When the buffer is full the oldest message is dropped to make room and reported
//!   as [`DeliveryFailure::BufferFull`]. A message larger than the whole byte bound is refused
//!   with [`RemoteFault::MailboxFull`].
//! - **Receiver.** A dedup window per sending node, keyed by (sender incarnation, sequence
//!   number), delivers each message once — retransmissions after a reconnect are acknowledged
//!   again but not delivered again. A new incarnation of the sender starts a new window. The
//!   window holds [`DeliveryConfig::dedup_window`] sequence numbers beyond the last contiguous
//!   one; keep it at least the peers' `max_unacked`.
//! - **Peer restart.** Messages still pending when the peer comes back with a new incarnation
//!   fail with [`DeliveryFailure::PeerRestarted`]: their targets died with the old life. The
//!   mesh learns the incarnation from [`Mesh::add_peer_node`] (before anything is retransmitted)
//!   or from the first ack of the new life.
//! - **Failures** reach every monitor registered with [`Mesh::monitor_delivery`] as a
//!   [`DeliveryFailed`].
//!
//! The guarantee is delivery to the receiving node's process layer: a message whose target no
//! longer exists, or whose target's mailbox is full (dropped and counted as for a cast,
//! [`BehaviorRegistry::dropped_full`](crate::BehaviorRegistry::dropped_full)), is still
//! acknowledged. A reliable frame received without a known sender ([`Mesh::handle_envelope`])
//! is delivered at-most-once (no window, no ack). Handlers must be idempotent where a message
//! can arrive twice: a sender that restarts forgets what it sent.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use id_effect::{Effect, fail};
use id_effect_process::{Addr, Pid, Process, SendError};
use serde::Serialize;

use crate::envelope::{ProcessEnvelope, ReliableMessage, RemoteFault};
use crate::id::NodeId;
use crate::mesh::Mesh;

/// Default [`DeliveryConfig::max_unacked`].
pub const DEFAULT_MAX_UNACKED: usize = 1024;
/// Default [`DeliveryConfig::max_unacked_bytes`] (8 MiB).
pub const DEFAULT_MAX_UNACKED_BYTES: usize = 8 * 1024 * 1024;
/// Default first retransmission delay.
pub const DEFAULT_INITIAL_BACKOFF: Duration = Duration::from_millis(200);
/// Default cap on the retransmission delay.
pub const DEFAULT_MAX_BACKOFF: Duration = Duration::from_secs(10);
/// Default [`DeliveryConfig::dedup_window`].
pub const DEFAULT_DEDUP_WINDOW: u64 = 4096;

/// Which guarantee remote casts and sends get by default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DeliveryMode {
  /// Lost on disconnect (ADR 0009, the default).
  #[default]
  AtMostOnce,
  /// Sequenced, acknowledged, retransmitted and deduplicated (see the [module docs](self)).
  AtLeastOnce,
}

/// Delivery settings of a mesh ([`Mesh::with_delivery`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeliveryConfig {
  /// Guarantee of [`Mesh::remote_cast`], [`Mesh::remote_info`], [`Mesh::cast`] and
  /// [`Mesh::send`]; the `*_reliable` sends are at-least-once whatever this says.
  pub mode: DeliveryMode,
  /// Unacknowledged messages kept per peer (at least 1).
  pub max_unacked: usize,
  /// Unacknowledged payload bytes kept per peer.
  pub max_unacked_bytes: usize,
  /// Delay before the first retransmission.
  pub initial_backoff: Duration,
  /// Cap on the retransmission delay (it doubles while no ack makes progress).
  pub max_backoff: Duration,
  /// Sequence numbers the receiver tracks beyond the last contiguous one, per sender.
  pub dedup_window: u64,
}

impl Default for DeliveryConfig {
  fn default() -> Self {
    Self {
      mode: DeliveryMode::AtMostOnce,
      max_unacked: DEFAULT_MAX_UNACKED,
      max_unacked_bytes: DEFAULT_MAX_UNACKED_BYTES,
      initial_backoff: DEFAULT_INITIAL_BACKOFF,
      max_backoff: DEFAULT_MAX_BACKOFF,
      dedup_window: DEFAULT_DEDUP_WINDOW,
    }
  }
}

impl DeliveryConfig {
  /// Defaults with [`DeliveryMode::AtLeastOnce`].
  pub fn at_least_once() -> Self {
    Self {
      mode: DeliveryMode::AtLeastOnce,
      ..Self::default()
    }
  }

  /// Keep at most `count` unacknowledged messages per peer (at least 1).
  pub fn with_max_unacked(mut self, count: usize) -> Self {
    self.max_unacked = count.max(1);
    self
  }

  /// Keep at most `bytes` unacknowledged payload bytes per peer.
  pub fn with_max_unacked_bytes(mut self, bytes: usize) -> Self {
    self.max_unacked_bytes = bytes;
    self
  }

  /// Retransmit after `initial`, doubling up to `max` while no ack makes progress.
  pub fn with_backoff(mut self, initial: Duration, max: Duration) -> Self {
    self.initial_backoff = initial;
    self.max_backoff = max.max(initial);
    self
  }

  /// Track `window` sequence numbers beyond the last contiguous one, per sender (at least 1).
  pub fn with_dedup_window(mut self, window: u64) -> Self {
    self.dedup_window = window.max(1);
    self
  }

  /// The retransmission delay after `round` rounds without progress.
  pub fn backoff(&self, round: u32) -> Duration {
    let factor = 1u32.checked_shl(round.min(31)).unwrap_or(u32::MAX);
    self
      .initial_backoff
      .saturating_mul(factor)
      .min(self.max_backoff)
  }
}

/// Why an at-least-once message was given up.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeliveryFailure {
  /// The peer's retransmit buffer was full; this, its oldest message, made room.
  BufferFull,
  /// The peer came back with a new incarnation before acknowledging it.
  PeerRestarted,
}

impl std::fmt::Display for DeliveryFailure {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::BufferFull => f.write_str("retransmit buffer full"),
      Self::PeerRestarted => f.write_str("peer restarted"),
    }
  }
}

/// An at-least-once message that will not be delivered, as reported to the sender's
/// [`DeliveryMonitor`]s.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeliveryFailed {
  /// The peer node it was sent to.
  pub peer: String,
  /// Its target process.
  pub to: Pid,
  /// Its sequence number.
  pub seq: u64,
  /// Why it was given up.
  pub reason: DeliveryFailure,
}

/// Receives [`DeliveryFailed`] reports ([`Mesh::monitor_delivery`]). Called without mesh locks
/// held, so it may use the mesh.
pub type DeliveryMonitor = Arc<dyn Fn(&DeliveryFailed) + Send + Sync>;

/// At-least-once send through a typed address.
pub trait SendReliable<P: Process> {
  /// Deliver `msg` to `handle_info` at-least-once. An address on `mesh`'s node is a local
  /// mailbox (ordered, never lost while the receiver lives) and is sent to directly; any other
  /// goes through [`Mesh::send_reliable`].
  fn send_reliable<R: Send + Sync + 'static>(
    &self,
    mesh: &Mesh,
    msg: P::Info,
  ) -> Effect<(), RemoteFault, R>;
}

impl<P: Process> SendReliable<P> for Addr<P>
where
  P::Info: Serialize,
{
  fn send_reliable<R: Send + Sync + 'static>(
    &self,
    mesh: &Mesh,
    msg: P::Info,
  ) -> Effect<(), RemoteFault, R> {
    if self.pid().node().as_str() == mesh.node_name() {
      return self.send(msg).map_err(|e| match e {
        SendError::Full => RemoteFault::MailboxFull,
        _ => RemoteFault::NoProc,
      });
    }
    match postcard::to_allocvec(&msg) {
      Ok(bytes) => mesh.send_reliable(self.pid(), bytes),
      Err(e) => fail(RemoteFault::Decode(e.to_string())),
    }
  }
}

struct Unacked {
  seq: u64,
  message: ReliableMessage,
  bytes: usize,
}

/// Sender state for one peer.
#[derive(Default)]
struct Outbox {
  last_seq: u64,
  peer_incarnation: Option<u128>,
  unacked: VecDeque<Unacked>,
  bytes: usize,
  round: u32,
  driving: bool,
}

impl Outbox {
  fn frame(&self, entry: &Unacked) -> ProcessEnvelope {
    let floor = self
      .unacked
      .front()
      .map_or(entry.seq, |u| u.seq.min(entry.seq));
    ProcessEnvelope::Reliable {
      seq: entry.seq,
      floor,
      message: entry.message.clone(),
    }
  }

  fn frames(&self) -> Vec<ProcessEnvelope> {
    self.unacked.iter().map(|u| self.frame(u)).collect()
  }

  fn failed(peer: &str, entry: Unacked, reason: DeliveryFailure) -> DeliveryFailed {
    DeliveryFailed {
      peer: peer.to_string(),
      to: entry.message.to().to_pid(),
      seq: entry.seq,
      reason,
    }
  }

  fn fail_all(&mut self, peer: &str, reason: DeliveryFailure) -> Vec<DeliveryFailed> {
    self.bytes = 0;
    self
      .unacked
      .drain(..)
      .map(|u| Self::failed(peer, u, reason.clone()))
      .collect()
  }

  /// Record the peer's incarnation; everything pending fails when it changed.
  fn learn_incarnation(&mut self, peer: &str, incarnation: u128) -> Vec<DeliveryFailed> {
    let restarted = self
      .peer_incarnation
      .is_some_and(|known| known != incarnation);
    self.peer_incarnation = Some(incarnation);
    if restarted {
      self.fail_all(peer, DeliveryFailure::PeerRestarted)
    } else {
      Vec::new()
    }
  }
}

/// Receiver state for one sending node.
struct Inbox {
  incarnation: u128,
  /// Every sequence number up to this one has been received.
  upto: u64,
  /// Received sequence numbers above `upto`.
  seen: BTreeSet<u64>,
}

impl Inbox {
  fn new(incarnation: u128) -> Self {
    Self {
      incarnation,
      upto: 0,
      seen: BTreeSet::new(),
    }
  }
}

/// What the receiver does with a reliable frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Received {
  /// First receipt: deliver, then acknowledge.
  Fresh,
  /// Already delivered: acknowledge only.
  Duplicate,
  /// Too far ahead of the window: neither (the sender retransmits it later).
  OutOfWindow,
}

/// A reliable message accepted into a peer's buffer.
pub(crate) struct Enqueued {
  /// The frame to transmit now.
  pub(crate) frame: ProcessEnvelope,
  /// The peer has no retransmit driver yet: start one.
  pub(crate) start_driver: bool,
  /// Older messages dropped to make room.
  pub(crate) evicted: Vec<DeliveryFailed>,
}

/// Unacked frames to send again after a reconnect.
pub(crate) struct Reconnected {
  pub(crate) failed: Vec<DeliveryFailed>,
  pub(crate) frames: Vec<ProcessEnvelope>,
  pub(crate) start_driver: bool,
}

/// The mesh's delivery state: config, per-peer outboxes, per-sender inboxes, monitors.
#[derive(Default)]
pub(crate) struct Delivery {
  config: Mutex<DeliveryConfig>,
  outboxes: Mutex<HashMap<String, Outbox>>,
  inboxes: Mutex<HashMap<String, Inbox>>,
  monitors: Mutex<Vec<DeliveryMonitor>>,
}

impl Delivery {
  pub(crate) fn config(&self) -> DeliveryConfig {
    self.config.lock().expect("delivery config").clone()
  }

  pub(crate) fn set_config(&self, config: DeliveryConfig) {
    *self.config.lock().expect("delivery config") = config;
  }

  pub(crate) fn add_monitor(&self, monitor: DeliveryMonitor) {
    self
      .monitors
      .lock()
      .expect("delivery monitors")
      .push(monitor);
  }

  /// Report `failures` to every monitor (call without other delivery locks held).
  pub(crate) fn notify(&self, failures: Vec<DeliveryFailed>) {
    if failures.is_empty() {
      return;
    }
    let monitors = self.monitors.lock().expect("delivery monitors").clone();
    for failure in &failures {
      for monitor in &monitors {
        monitor(failure);
      }
    }
  }

  /// Buffer `message` for `peer`, evicting the oldest messages over the bounds.
  pub(crate) fn enqueue(
    &self,
    peer: &str,
    message: ReliableMessage,
  ) -> Result<Enqueued, RemoteFault> {
    let config = self.config();
    let bytes = message.payload_len();
    if bytes > config.max_unacked_bytes {
      return Err(RemoteFault::MailboxFull);
    }
    let mut outboxes = self.outboxes.lock().expect("outboxes");
    let outbox = outboxes.entry(peer.to_string()).or_default();
    let mut evicted = Vec::new();
    while outbox.unacked.len() >= config.max_unacked
      || outbox.bytes + bytes > config.max_unacked_bytes
    {
      let Some(oldest) = outbox.unacked.pop_front() else {
        break;
      };
      outbox.bytes -= oldest.bytes;
      evicted.push(Outbox::failed(peer, oldest, DeliveryFailure::BufferFull));
    }
    outbox.last_seq += 1;
    let entry = Unacked {
      seq: outbox.last_seq,
      message,
      bytes,
    };
    let frame = outbox.frame(&entry);
    outbox.bytes += bytes;
    outbox.unacked.push_back(entry);
    let start_driver = !outbox.driving;
    outbox.driving = true;
    Ok(Enqueued {
      frame,
      start_driver,
      evicted,
    })
  }

  /// `from` acknowledged `seq` and everything up to `upto`.
  pub(crate) fn on_ack(&self, from: &NodeId, upto: u64, seq: u64) -> Vec<DeliveryFailed> {
    let mut outboxes = self.outboxes.lock().expect("outboxes");
    let Some(outbox) = outboxes.get_mut(from.name()) else {
      return Vec::new();
    };
    let failed = outbox.learn_incarnation(from.name(), from.incarnation());
    if !failed.is_empty() {
      // The ack is the new life's: nothing it covers is still pending.
      return failed;
    }
    let before = outbox.unacked.len();
    outbox.unacked.retain(|u| u.seq > upto && u.seq != seq);
    if outbox.unacked.len() != before {
      outbox.bytes = outbox.unacked.iter().map(|u| u.bytes).sum();
      outbox.round = 0;
    }
    Vec::new()
  }

  /// `peer` (re)connected, as `incarnation` when known: fail what its previous life never
  /// acknowledged, and hand back the rest to send again.
  pub(crate) fn on_connect(&self, peer: &str, incarnation: Option<u128>) -> Reconnected {
    let mut outboxes = self.outboxes.lock().expect("outboxes");
    let outbox = outboxes.entry(peer.to_string()).or_default();
    let failed = match incarnation {
      Some(incarnation) => outbox.learn_incarnation(peer, incarnation),
      None => Vec::new(),
    };
    outbox.round = 0;
    let frames = outbox.frames();
    let start_driver = !frames.is_empty() && !outbox.driving;
    if start_driver {
      outbox.driving = true;
    }
    Reconnected {
      failed,
      frames,
      start_driver,
    }
  }

  /// One retransmission round for `peer`: the frames to send again and the delay before the
  /// next round, or `None` (the driver stops) when nothing is unacknowledged.
  pub(crate) fn tick(&self, peer: &str) -> Option<(Vec<ProcessEnvelope>, Duration)> {
    let config = self.config();
    let mut outboxes = self.outboxes.lock().expect("outboxes");
    let outbox = outboxes.get_mut(peer)?;
    if outbox.unacked.is_empty() {
      outbox.driving = false;
      return None;
    }
    outbox.round = outbox.round.saturating_add(1);
    Some((outbox.frames(), config.backoff(outbox.round)))
  }

  /// Classify reliable frame `seq` from `from` (whose buffer starts at `floor`) and record it;
  /// returns the classification and the contiguous receipt to acknowledge.
  pub(crate) fn receive(&self, from: &NodeId, seq: u64, floor: u64) -> (Received, u64) {
    let window = self.config().dedup_window;
    let mut inboxes = self.inboxes.lock().expect("inboxes");
    let inbox = inboxes
      .entry(from.name().to_string())
      .or_insert_with(|| Inbox::new(from.incarnation()));
    if inbox.incarnation != from.incarnation() {
      *inbox = Inbox::new(from.incarnation());
    }
    // The sender gave up on everything below `floor`: slide past it.
    if floor > inbox.upto.saturating_add(1) {
      inbox.upto = floor - 1;
      inbox.seen = inbox.seen.split_off(&floor);
    }
    let received = if seq <= inbox.upto || inbox.seen.contains(&seq) {
      Received::Duplicate
    } else if seq - inbox.upto > window {
      Received::OutOfWindow
    } else {
      inbox.seen.insert(seq);
      while inbox.upto < u64::MAX && inbox.seen.remove(&(inbox.upto + 1)) {
        inbox.upto += 1;
      }
      Received::Fresh
    };
    (received, inbox.upto)
  }

  /// Unacknowledged messages buffered for `peer`.
  pub(crate) fn unacked(&self, peer: &str) -> usize {
    self
      .outboxes
      .lock()
      .expect("outboxes")
      .get(peer)
      .map_or(0, |o| o.unacked.len())
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::envelope::PidWire;

  fn message(id: u64, len: usize) -> ReliableMessage {
    ReliableMessage::Cast {
      to: PidWire {
        node: "b@test".to_string(),
        id,
        incarnation: 1,
      },
      msg: vec![0; len],
      trace: Vec::new(),
    }
  }

  fn delivery(config: DeliveryConfig) -> Delivery {
    let d = Delivery::default();
    d.set_config(config);
    d
  }

  fn seq_of(frame: &ProcessEnvelope) -> (u64, u64) {
    match frame {
      ProcessEnvelope::Reliable { seq, floor, .. } => (*seq, *floor),
      other => panic!("not reliable: {other:?}"),
    }
  }

  #[test]
  fn config_defaults_and_builders() {
    let config = DeliveryConfig::default();
    assert_eq!(config.mode, DeliveryMode::AtMostOnce);
    assert_eq!(DeliveryMode::default(), DeliveryMode::AtMostOnce);
    assert_eq!(config.max_unacked, DEFAULT_MAX_UNACKED);
    let tuned = DeliveryConfig::at_least_once()
      .with_max_unacked(0)
      .with_max_unacked_bytes(10)
      .with_backoff(Duration::from_millis(10), Duration::from_millis(5))
      .with_dedup_window(0);
    assert_eq!(tuned.mode, DeliveryMode::AtLeastOnce);
    assert_eq!(tuned.max_unacked, 1);
    assert_eq!(tuned.max_unacked_bytes, 10);
    assert_eq!(tuned.max_backoff, Duration::from_millis(10));
    assert_eq!(tuned.dedup_window, 1);
  }

  #[test]
  fn backoff_doubles_up_to_the_cap() {
    let config =
      DeliveryConfig::default().with_backoff(Duration::from_millis(10), Duration::from_millis(70));
    let delays: Vec<_> = (0..5).map(|r| config.backoff(r).as_millis()).collect();
    assert_eq!(delays, [10, 20, 40, 70, 70]);
    assert_eq!(config.backoff(u32::MAX), Duration::from_millis(70));
  }

  #[test]
  fn sequence_numbers_start_at_one_per_peer() {
    let d = delivery(DeliveryConfig::at_least_once());
    let first = d.enqueue("b", message(1, 1)).expect("enqueue");
    assert!(first.start_driver);
    assert_eq!(seq_of(&first.frame), (1, 1));
    let second = d.enqueue("b", message(1, 1)).expect("enqueue");
    assert!(!second.start_driver, "one driver per peer");
    assert_eq!(seq_of(&second.frame), (2, 1));
    assert_eq!(
      seq_of(&d.enqueue("c", message(1, 1)).expect("c").frame),
      (1, 1)
    );
    assert_eq!(d.unacked("b"), 2);
  }

  #[test]
  fn byte_bound_evicts_oldest_and_refuses_oversized() {
    let d = delivery(DeliveryConfig::at_least_once().with_max_unacked_bytes(10));
    d.enqueue("b", message(1, 4)).expect("1");
    d.enqueue("b", message(2, 4)).expect("2");
    let third = d.enqueue("b", message(3, 4)).expect("3");
    assert_eq!(third.evicted.len(), 1);
    assert_eq!(third.evicted[0].seq, 1);
    assert_eq!(third.evicted[0].reason, DeliveryFailure::BufferFull);
    assert_eq!(
      seq_of(&third.frame),
      (3, 2),
      "floor moved past the evicted one"
    );
    assert!(matches!(
      d.enqueue("b", message(4, 11)),
      Err(RemoteFault::MailboxFull)
    ));
  }

  #[test]
  fn ack_removes_covered_messages_and_resets_backoff() {
    let d = delivery(DeliveryConfig::at_least_once());
    for _ in 0..4 {
      d.enqueue("b", message(1, 1)).expect("enqueue");
    }
    let (frames, _) = d.tick("b").expect("pending");
    assert_eq!(frames.len(), 4);
    let b = NodeId::from_parts("b", 7);
    assert!(d.on_ack(&b, 1, 3).is_empty());
    assert_eq!(d.unacked("b"), 2, "1 and 3 acked");
    assert!(d.on_ack(&NodeId::from_parts("zz", 1), 9, 9).is_empty());
    assert!(d.on_ack(&b, 4, 4).is_empty());
    assert_eq!(d.unacked("b"), 0);
    assert!(d.tick("b").is_none(), "driver stops when drained");
    assert!(d.tick("nobody").is_none());
  }

  #[test]
  fn new_incarnation_fails_everything_pending() {
    let d = delivery(DeliveryConfig::at_least_once());
    let r = d.on_connect("b", Some(1));
    assert!(r.failed.is_empty() && r.frames.is_empty() && !r.start_driver);
    d.enqueue("b", message(5, 1)).expect("enqueue");
    let same = d.on_connect("b", Some(1));
    assert_eq!(same.frames.len(), 1, "same life: retransmit");
    let restarted = d.on_connect("b", Some(2));
    assert_eq!(restarted.failed.len(), 1);
    assert_eq!(restarted.failed[0].reason, DeliveryFailure::PeerRestarted);
    assert_eq!(restarted.failed[0].to.local_id(), 5);
    assert!(restarted.frames.is_empty());

    d.enqueue("b", message(6, 1)).expect("enqueue");
    let failed = d.on_ack(&NodeId::from_parts("b", 3), 99, 99);
    assert_eq!(
      failed.len(),
      1,
      "an ack from a newer life fails what is pending"
    );
    assert_eq!(d.unacked("b"), 0);
  }

  #[test]
  fn receive_window_dedups_and_slides() {
    let d = delivery(DeliveryConfig::at_least_once().with_dedup_window(4));
    let a = NodeId::from_parts("a", 1);
    assert_eq!(d.receive(&a, 1, 1), (Received::Fresh, 1));
    assert_eq!(d.receive(&a, 1, 1), (Received::Duplicate, 1));
    assert_eq!(d.receive(&a, 3, 1), (Received::Fresh, 1));
    assert_eq!(d.receive(&a, 3, 1), (Received::Duplicate, 1));
    assert_eq!(d.receive(&a, 2, 1), (Received::Fresh, 3));
    assert_eq!(d.receive(&a, 8, 1), (Received::OutOfWindow, 3));
    assert_eq!(d.receive(&a, 0, 0), (Received::Duplicate, 3));
    // The sender gave up on 4..=9: the window slides to 10.
    assert_eq!(d.receive(&a, 10, 10), (Received::Fresh, 10));
    // A new life of the sender starts over.
    let reborn = NodeId::from_parts("a", 2);
    assert_eq!(d.receive(&reborn, 1, 1), (Received::Fresh, 1));
  }

  #[test]
  fn monitors_get_every_failure() {
    let d = Delivery::default();
    d.notify(Vec::new());
    let (tx, rx) = flume::unbounded();
    d.add_monitor(Arc::new(move |f: &DeliveryFailed| {
      let _ = tx.send(f.clone());
    }));
    let failure = DeliveryFailed {
      peer: "b".to_string(),
      to: message(1, 0).to().to_pid(),
      seq: 1,
      reason: DeliveryFailure::BufferFull,
    };
    d.notify(vec![failure.clone()]);
    assert_eq!(rx.try_recv().expect("reported"), failure);
    assert_eq!(
      DeliveryFailure::BufferFull.to_string(),
      "retransmit buffer full"
    );
    assert_eq!(DeliveryFailure::PeerRestarted.to_string(), "peer restarted");
  }
}
