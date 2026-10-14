//! [`InMemoryTransport`] — deterministic in-process transport with
//! scripted partitions.
//!
//! All nodes in a test share one [`MemoryNetwork`]. Each node's transport
//! carries a **local identity** (its listen address), so partitions can be
//! scripted between node addresses and affect every connection between the
//! two nodes — dialed or accepted. Partitions are imposed and healed
//! synchronously, making netsplit tests fully deterministic (ADR 0009
//! § verification).
//!
//! Connection semantics mirror TCP: a partition kills every connection that
//! existed when it was imposed, and those connections stay dead after
//! [`MemoryNetwork::heal`] (only new dials succeed). After a close or a
//! partition, frames that had already arrived are still delivered by
//! `recv` before it reports [`TransportError::Closed`]; sends fail at once.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use bytes::Bytes;

use super::{Connection, Listener, Transport, TransportError};

/// One half of a duplex in-memory pipe.
struct Pipe {
  tx: flume::Sender<Bytes>,
  rx: flume::Receiver<Bytes>,
  /// Node addresses of both endpoints, used for partition checks on every
  /// frame.
  local: String,
  remote: String,
  net: Arc<NetworkInner>,
  /// Shared by both halves: set by `close` on either end.
  closed: Arc<AtomicBool>,
  /// Partitions imposed on this address pair before the connection was
  /// made; any later partition kills it for good.
  epoch: u64,
}

impl Pipe {
  fn is_dead(&self) -> bool {
    self.closed.load(Ordering::Acquire) || self.net.cuts(&self.local, &self.remote) > self.epoch
  }
}

#[async_trait::async_trait]
impl Connection for Pipe {
  async fn send(&self, frame: Bytes) -> Result<(), TransportError> {
    if self.is_dead() {
      return Err(TransportError::Closed);
    }
    self
      .tx
      .send_async(frame)
      .await
      .map_err(|_| TransportError::Closed)
  }

  async fn recv(&self) -> Result<Bytes, TransportError> {
    // Poll both the queue and the partition/close state: a partition or a
    // close must also fail receivers that are already parked waiting, but
    // only once every frame that already arrived has been handed out.
    loop {
      if let Ok(frame) = self.rx.try_recv() {
        return Ok(frame);
      }
      if self.is_dead() {
        return Err(TransportError::Closed);
      }
      match tokio::time::timeout(std::time::Duration::from_millis(10), self.rx.recv_async()).await {
        Ok(Ok(frame)) => return Ok(frame),
        Ok(Err(_)) => return Err(TransportError::Closed),
        Err(_elapsed) => continue,
      }
    }
  }

  async fn close(&self) {
    // Both ends share the flag, so the peer observes `Closed` on its next
    // operation (or within one poll tick if parked in `recv`).
    self.closed.store(true, Ordering::Release);
  }

  fn peer_addr(&self) -> String {
    self.remote.clone()
  }
}

struct MemoryListener {
  addr: String,
  inbound: flume::Receiver<Box<dyn Connection>>,
}

#[async_trait::async_trait]
impl Listener for MemoryListener {
  async fn accept(&self) -> Result<Box<dyn Connection>, TransportError> {
    self
      .inbound
      .recv_async()
      .await
      .map_err(|_| TransportError::Closed)
  }

  fn local_addr(&self) -> String {
    self.addr.clone()
  }
}

struct NetworkInner {
  listeners: Mutex<HashMap<String, flume::Sender<Box<dyn Connection>>>>,
  /// Unordered pairs of currently partitioned node addresses.
  partitions: Mutex<HashSet<(String, String)>>,
  /// How many partitions each unordered pair has ever seen.
  cut_counts: Mutex<HashMap<(String, String), u64>>,
}

impl NetworkInner {
  fn key(a: &str, b: &str) -> (String, String) {
    if a <= b {
      (a.to_string(), b.to_string())
    } else {
      (b.to_string(), a.to_string())
    }
  }

  fn is_partitioned(&self, a: &str, b: &str) -> bool {
    self
      .partitions
      .lock()
      .expect("partitions mutex poisoned")
      .contains(&Self::key(a, b))
  }

  fn cuts(&self, a: &str, b: &str) -> u64 {
    self
      .cut_counts
      .lock()
      .expect("cuts mutex poisoned")
      .get(&Self::key(a, b))
      .copied()
      .unwrap_or(0)
  }
}

/// Shared in-memory “network”: create one per test, hand a
/// [`InMemoryTransport`] to each simulated node via
/// [`MemoryNetwork::transport`].
#[derive(Clone)]
pub struct MemoryNetwork {
  inner: Arc<NetworkInner>,
}

impl Default for MemoryNetwork {
  fn default() -> Self {
    Self::new()
  }
}

impl MemoryNetwork {
  /// Fresh, fully connected network.
  pub fn new() -> Self {
    Self {
      inner: Arc::new(NetworkInner {
        listeners: Mutex::new(HashMap::new()),
        partitions: Mutex::new(HashSet::new()),
        cut_counts: Mutex::new(HashMap::new()),
      }),
    }
  }

  /// Transport endpoint for the simulated node whose address is `local`.
  /// Use the same string the node passes to `listen`.
  pub fn transport(&self, local: impl Into<String>) -> InMemoryTransport {
    InMemoryTransport {
      local: local.into(),
      net: Arc::clone(&self.inner),
    }
  }

  /// Cut the link between two node addresses (both directions). Existing
  /// connections die permanently: sends fail, and `recv` reports
  /// [`TransportError::Closed`] once already-arrived frames are drained.
  /// New dials fail until [`MemoryNetwork::heal`].
  pub fn partition(&self, a: &str, b: &str) {
    let key = NetworkInner::key(a, b);
    *self
      .inner
      .cut_counts
      .lock()
      .expect("cuts mutex poisoned")
      .entry(key.clone())
      .or_insert(0) += 1;
    self
      .inner
      .partitions
      .lock()
      .expect("partitions mutex poisoned")
      .insert(key);
  }

  /// Heal the link between two node addresses: new dials succeed again.
  /// Connections killed by the partition stay dead (like real TCP).
  pub fn heal(&self, a: &str, b: &str) {
    self
      .inner
      .partitions
      .lock()
      .expect("partitions mutex poisoned")
      .remove(&NetworkInner::key(a, b));
  }
}

/// The per-node endpoint of a [`MemoryNetwork`].
#[derive(Clone)]
pub struct InMemoryTransport {
  local: String,
  net: Arc<NetworkInner>,
}

#[async_trait::async_trait]
impl Transport for InMemoryTransport {
  async fn connect(&self, addr: &str) -> Result<Box<dyn Connection>, TransportError> {
    // Read the epoch first: a partition racing this dial then kills the
    // new connection instead of letting it slip through.
    let epoch = self.net.cuts(&self.local, addr);
    if self.net.is_partitioned(&self.local, addr) {
      return Err(TransportError::Connect("partitioned".to_string()));
    }
    let sender = {
      let listeners = self.net.listeners.lock().expect("listeners mutex poisoned");
      listeners.get(addr).cloned()
    };
    let Some(sender) = sender else {
      return Err(TransportError::Connect(format!("no listener at {addr}")));
    };

    let (a_tx, a_rx) = flume::unbounded::<Bytes>();
    let (b_tx, b_rx) = flume::unbounded::<Bytes>();
    let closed = Arc::new(AtomicBool::new(false));

    let dialer: Box<dyn Connection> = Box::new(Pipe {
      tx: a_tx,
      rx: b_rx,
      local: self.local.clone(),
      remote: addr.to_string(),
      net: Arc::clone(&self.net),
      closed: Arc::clone(&closed),
      epoch,
    });
    let accepted: Box<dyn Connection> = Box::new(Pipe {
      tx: b_tx,
      rx: a_rx,
      local: addr.to_string(),
      remote: self.local.clone(),
      net: Arc::clone(&self.net),
      closed,
      epoch,
    });

    sender
      .send_async(accepted)
      .await
      .map_err(|_| TransportError::Connect("listener dropped".to_string()))?;
    Ok(dialer)
  }

  async fn listen(&self, addr: &str) -> Result<Box<dyn Listener>, TransportError> {
    let (tx, rx) = flume::unbounded();
    let mut listeners = self.net.listeners.lock().expect("listeners mutex poisoned");
    // A dropped listener frees its address, so a restarted node can rebind.
    if listeners
      .get(addr)
      .is_some_and(|live| !live.is_disconnected())
    {
      return Err(TransportError::Bind(format!("{addr} already bound")));
    }
    listeners.insert(addr.to_string(), tx);
    Ok(Box::new(MemoryListener {
      addr: addr.to_string(),
      inbound: rx,
    }))
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  async fn connected(
    net: &MemoryNetwork,
    from: &str,
    to: &str,
  ) -> (Box<dyn Connection>, Box<dyn Connection>, Box<dyn Listener>) {
    let listener = net.transport(to).listen(to).await.expect("bind");
    let dial = net.transport(from).connect(to).await.expect("dial");
    let accepted = listener.accept().await.expect("accept");
    (dial, accepted, listener)
  }

  #[tokio::test]
  async fn double_bind_is_refused() {
    let net = MemoryNetwork::new();
    let t = net.transport("a");
    let _held = t.listen("a").await.expect("first bind");
    assert!(matches!(t.listen("a").await, Err(TransportError::Bind(_))));
  }

  #[tokio::test]
  async fn dropped_listener_frees_its_address() {
    let net = MemoryNetwork::new();
    let t = net.transport("a");
    drop(t.listen("a").await.expect("first bind"));
    let again = t.listen("a").await.expect("rebind after drop");
    assert_eq!(again.local_addr(), "a");
  }

  #[tokio::test]
  async fn dial_without_listener_fails() {
    let net = MemoryNetwork::new();
    let err = net
      .transport("a")
      .connect("nowhere")
      .await
      .err()
      .expect("no listener");
    assert!(matches!(err, TransportError::Connect(m) if m.contains("no listener")));
  }

  #[tokio::test]
  async fn addresses_identify_both_ends() {
    let net = MemoryNetwork::default();
    let (dial, accepted, listener) = connected(&net, "a", "b").await;
    assert_eq!(listener.local_addr(), "b");
    assert_eq!(dial.peer_addr(), "b");
    assert_eq!(accepted.peer_addr(), "a");
  }

  #[tokio::test]
  async fn frames_flow_both_ways_in_order() {
    let net = MemoryNetwork::new();
    let (dial, accepted, _l) = connected(&net, "a", "b").await;
    for i in 0..5u8 {
      accepted.send(Bytes::from(vec![i])).await.expect("send");
    }
    for i in 0..5u8 {
      assert_eq!(dial.recv().await.expect("recv"), Bytes::from(vec![i]));
    }
  }

  #[tokio::test]
  async fn dropping_one_end_closes_the_other() {
    let net = MemoryNetwork::new();
    let (dial, accepted, _l) = connected(&net, "a", "b").await;
    drop(dial);
    assert!(matches!(accepted.recv().await, Err(TransportError::Closed)));
    assert!(matches!(
      accepted.send(Bytes::from_static(b"x")).await,
      Err(TransportError::Closed)
    ));
  }

  #[tokio::test]
  async fn close_is_observed_by_the_peer_and_is_idempotent() {
    let net = MemoryNetwork::new();
    let (dial, accepted, _l) = connected(&net, "a", "b").await;
    dial.close().await;
    dial.close().await;
    assert!(matches!(accepted.recv().await, Err(TransportError::Closed)));
    assert!(matches!(
      dial.send(Bytes::from_static(b"x")).await,
      Err(TransportError::Closed)
    ));
  }

  #[tokio::test]
  async fn partition_wakes_a_parked_receiver() {
    let net = MemoryNetwork::new();
    let (_dial, accepted, _l) = connected(&net, "a", "b").await;
    let parked = tokio::spawn(async move { accepted.recv().await });
    tokio::task::yield_now().await;
    net.partition("a", "b");
    let outcome = parked.await.expect("join");
    assert!(matches!(outcome, Err(TransportError::Closed)));
  }

  #[tokio::test]
  async fn frames_sent_before_close_are_still_received() {
    let net = MemoryNetwork::new();
    let (dial, accepted, _l) = connected(&net, "a", "b").await;
    dial.send(Bytes::from_static(b"one")).await.expect("send");
    dial.send(Bytes::from_static(b"two")).await.expect("send");
    dial.close().await;
    assert_eq!(
      accepted.recv().await.expect("one"),
      Bytes::from_static(b"one")
    );
    assert_eq!(
      accepted.recv().await.expect("two"),
      Bytes::from_static(b"two")
    );
    assert!(matches!(accepted.recv().await, Err(TransportError::Closed)));
  }

  #[tokio::test]
  async fn frames_that_arrived_before_a_partition_are_still_received() {
    let net = MemoryNetwork::new();
    let (dial, accepted, _l) = connected(&net, "a", "b").await;
    dial
      .send(Bytes::from_static(b"in flight"))
      .await
      .expect("send");
    net.partition("a", "b");
    assert!(dial.send(Bytes::from_static(b"late")).await.is_err());
    assert_eq!(
      accepted.recv().await.expect("buffered frame"),
      Bytes::from_static(b"in flight")
    );
    assert!(matches!(accepted.recv().await, Err(TransportError::Closed)));
  }

  /// Regression: healing used to revive connections the partition had
  /// killed (if neither end touched them meanwhile), contradicting the docs.
  #[tokio::test]
  async fn partition_killed_connections_stay_dead_after_heal() {
    let net = MemoryNetwork::new();
    let (dial, accepted, listener) = connected(&net, "a", "b").await;
    net.partition("a", "b");
    net.heal("a", "b");
    assert!(dial.send(Bytes::from_static(b"x")).await.is_err());
    assert!(matches!(accepted.recv().await, Err(TransportError::Closed)));
    // New dials work after the heal.
    let fresh = net.transport("a").connect("b").await.expect("redial");
    let fresh_accepted = listener.accept().await.expect("accept");
    fresh.send(Bytes::from_static(b"ok")).await.expect("send");
    assert_eq!(
      fresh_accepted.recv().await.expect("recv"),
      Bytes::from_static(b"ok")
    );
  }

  #[tokio::test]
  async fn partition_is_symmetric() {
    let net = MemoryNetwork::new();
    let _la = net.transport("a").listen("a").await.expect("bind a");
    let _lb = net.transport("b").listen("b").await.expect("bind b");
    net.partition("b", "a");
    assert!(net.transport("a").connect("b").await.is_err());
    assert!(net.transport("b").connect("a").await.is_err());
  }

  #[tokio::test]
  async fn partition_leaves_other_links_alone() {
    let net = MemoryNetwork::new();
    let (ab, _ba, _lb) = connected(&net, "a", "b").await;
    let (ac, ca, _lc) = connected(&net, "a", "c").await;
    net.partition("a", "b");
    assert!(ab.send(Bytes::from_static(b"x")).await.is_err());
    ac.send(Bytes::from_static(b"ok"))
      .await
      .expect("a-c unaffected");
    assert_eq!(ca.recv().await.expect("recv"), Bytes::from_static(b"ok"));
  }

  #[tokio::test]
  async fn dial_to_dropped_listener_fails() {
    let net = MemoryNetwork::new();
    drop(net.transport("b").listen("b").await.expect("bind"));
    let err = net
      .transport("a")
      .connect("b")
      .await
      .err()
      .expect("listener gone");
    assert!(matches!(err, TransportError::Connect(m) if m == "listener dropped"));
  }

  #[tokio::test]
  async fn heal_of_an_intact_link_is_a_noop() {
    let net = MemoryNetwork::new();
    let (dial, accepted, _l) = connected(&net, "a", "b").await;
    net.heal("a", "b");
    dial.send(Bytes::from_static(b"still")).await.expect("send");
    assert_eq!(
      accepted.recv().await.expect("recv"),
      Bytes::from_static(b"still")
    );
  }
}
