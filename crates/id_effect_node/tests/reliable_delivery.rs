//! At-least-once delivery (0.11.0 D19), remote trace context (dist.remote#8) and the protocol v3
//! bump, over two meshes joined by switchable in-process links (no sockets).

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use id_effect::{Cause, Effect, Exit, ExitKind, Never, Span, SpanHook, succeed, sync};
use id_effect_node::handshake::{Cookie, HandshakeError};
use id_effect_node::session::{Session, SessionError};
use id_effect_node::transport::Transport;
use id_effect_node::transport::memory::MemoryNetwork;
use id_effect_node::wire::{Frame, HandshakeMsg, PROTOCOL_VERSION, decode, encode};
use id_effect_node::{
  BehaviorRegistry, DeliveryConfig, DeliveryFailed, DeliveryFailure, DeliveryMode, Mesh, NodeId,
  PeerSender, ProcessEnvelope, RemoteFault, SendReliable, W3cTraceContext, traceparent_of,
};
use id_effect_process::{Info, Next, NodeName, Pid, Process, ProcessCtx, ProcessNode};
use serde::{Deserialize, Serialize};

const WAIT: Duration = Duration::from_secs(5);

fn run<A: Send + 'static>(eff: Effect<A, RemoteFault>) -> Result<A, RemoteFault> {
  match eff.block_on() {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(f)) => Err(f),
    Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
  }
}

/// Polls `done` until it holds or [`WAIT`] passes.
fn eventually(done: impl Fn() -> bool) -> bool {
  let deadline = Instant::now() + WAIT;
  while Instant::now() < deadline {
    if done() {
      return true;
    }
    std::thread::sleep(Duration::from_millis(5));
  }
  done()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
enum CounterCall {
  Get,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
enum CounterCast {
  Add(u32),
}

/// Sums every cast and info it receives.
struct Counter {
  n: u32,
}

impl Process for Counter {
  type Args = ();
  type Env = ();
  type Call = CounterCall;
  type Reply = u32;
  type Cast = CounterCast;
  type Info = u32;
  type Error = Never;

  fn init(_: (), _: ProcessCtx) -> Effect<Self, Never, ()> {
    succeed(Counter { n: 0 })
  }

  fn handle_call(self, _: CounterCall, _: ProcessCtx) -> Effect<(u32, Next<Self>), Never, ()> {
    succeed((self.n, Next::Continue(self)))
  }

  fn handle_cast(mut self, msg: CounterCast, _: ProcessCtx) -> Effect<Next<Self>, Never, ()> {
    sync(move || {
      let CounterCast::Add(x) = msg;
      self.n += x;
      Next::Continue(self)
    })
  }

  fn handle_info(mut self, msg: Info<u32>, _: ProcessCtx) -> Effect<Next<Self>, Never, ()> {
    sync(move || {
      if let Info::User(x) = msg {
        self.n += x;
      }
      Next::Continue(self)
    })
  }
}

fn add(x: u32) -> Vec<u8> {
  postcard::to_allocvec(&CounterCast::Add(x)).expect("encode")
}

/// One direction of a link: up or down, counting the reliable frames it carried.
#[derive(Clone, Default)]
struct Wire {
  down: Arc<AtomicBool>,
  reliable: Arc<AtomicUsize>,
}

impl Wire {
  fn set_up(&self, up: bool) {
    self.down.store(!up, Ordering::SeqCst);
  }

  /// A sender delivering into `to` as `from` while the wire is up.
  fn sender(&self, from: NodeId, to: Mesh) -> PeerSender {
    let wire = self.clone();
    Arc::new(move |envelope| {
      if wire.down.load(Ordering::SeqCst) {
        return Err(RemoteFault::NoProc);
      }
      if matches!(envelope, ProcessEnvelope::Reliable { .. }) {
        wire.reliable.fetch_add(1, Ordering::SeqCst);
      }
      to.handle_envelope_from(&from, envelope)
    })
  }
}

/// Two meshes `a@test` and `b@test` joined by `a_to_b` and `b_to_a`.
struct Pair {
  a: Mesh,
  b: Mesh,
  a_id: NodeId,
  b_id: NodeId,
  a_to_b: Wire,
  b_to_a: Wire,
}

impl Pair {
  fn new(config: DeliveryConfig) -> Self {
    Self::with(config, |m| m)
  }

  fn with(config: DeliveryConfig, b_setup: impl FnOnce(Mesh) -> Mesh) -> Self {
    let a_id = NodeId::from_parts("a@test", 1);
    let b_id = NodeId::from_parts("b@test", 1);
    let a = mesh(&a_id).with_delivery(config.clone());
    let b = b_setup(mesh(&b_id).with_delivery(config));
    let pair = Self {
      a,
      b,
      a_id,
      b_id,
      a_to_b: Wire::default(),
      b_to_a: Wire::default(),
    };
    pair.reconnect();
    pair
  }

  /// Both directions (re)connected, as after a new session.
  fn reconnect(&self) {
    self.a_to_b.set_up(true);
    self.b_to_a.set_up(true);
    self.b.add_peer_node(
      &self.a_id,
      self.b_to_a.sender(self.b_id.clone(), self.a.clone()),
    );
    self.a.add_peer_node(
      &self.b_id,
      self.a_to_b.sender(self.a_id.clone(), self.b.clone()),
    );
  }

  fn spawn_counter(&self) -> Pid {
    run(self.a.remote_spawn("b@test", "counter", unit(), None, WAIT)).expect("spawn")
  }

  fn count(&self, pid: &Pid) -> u32 {
    let call = postcard::to_allocvec(&CounterCall::Get).expect("encode");
    let reply = run(self.a.remote_call(pid, call, WAIT)).expect("get");
    postcard::from_bytes(&reply).expect("decode")
  }
}

fn mesh(id: &NodeId) -> Mesh {
  let mut registry = BehaviorRegistry::new();
  registry.register::<Counter>("counter");
  Mesh::new(
    id.clone(),
    ProcessNode::new(NodeName::new(id.name())),
    registry,
  )
}

fn unit() -> Vec<u8> {
  postcard::to_allocvec(&()).expect("unit")
}

fn fast() -> DeliveryConfig {
  DeliveryConfig::at_least_once().with_backoff(Duration::from_millis(10), Duration::from_millis(40))
}

/// No retransmission timer within a test's lifetime: only reconnects resend.
fn no_timer() -> DeliveryConfig {
  DeliveryConfig::at_least_once().with_backoff(Duration::from_secs(600), Duration::from_secs(600))
}

fn failures(mesh: &Mesh) -> Arc<Mutex<Vec<DeliveryFailed>>> {
  let seen = Arc::new(Mutex::new(Vec::new()));
  let sink = Arc::clone(&seen);
  mesh.monitor_delivery(Arc::new(move |f: &DeliveryFailed| {
    sink.lock().expect("failures").push(f.clone());
  }));
  seen
}

#[test]
fn reliable_send_survives_connection_drop() {
  let pair = Pair::new(fast());
  let pid = pair.spawn_counter();

  // The connection drops: the message is buffered, not lost.
  pair.a_to_b.set_up(false);
  run(pair.a.cast_reliable(&pid, add(5))).expect("buffered");
  assert_eq!(pair.a.unacked("b@test"), 1);

  // A new session resends it.
  pair.reconnect();
  assert_eq!(pair.count(&pid), 5);
  assert_eq!(pair.a.unacked("b@test"), 0);

  // Without a reconnect, the backoff timer resends it once the link is back.
  pair.a_to_b.set_up(false);
  run(
    pair
      .a
      .send_reliable(&pid, postcard::to_allocvec(&2u32).expect("info")),
  )
  .expect("buffered");
  pair.a_to_b.set_up(true);
  assert!(
    eventually(|| pair.a.unacked("b@test") == 0),
    "never retransmitted"
  );
  assert_eq!(pair.count(&pid), 7);
}

#[test]
fn duplicates_suppressed_after_reconnect() {
  let pair = Pair::new(no_timer());
  let pid = pair.spawn_counter();

  // b receives every message but its acks are lost.
  pair.b_to_a.set_up(false);
  for _ in 0..3 {
    run(pair.a.cast_reliable(&pid, add(1))).expect("sent");
  }
  assert_eq!(pair.a.unacked("b@test"), 3);
  pair.b_to_a.set_up(true);
  assert_eq!(pair.count(&pid), 3);

  // The reconnect resends all three; b acknowledges them without delivering them again.
  let before = pair.a_to_b.reliable.load(Ordering::SeqCst);
  pair.reconnect();
  assert_eq!(pair.a_to_b.reliable.load(Ordering::SeqCst) - before, 3);
  assert_eq!(pair.a.unacked("b@test"), 0);
  assert_eq!(pair.count(&pid), 3);

  // A later message is still delivered once.
  run(pair.a.cast_reliable(&pid, add(10))).expect("sent");
  assert_eq!(pair.count(&pid), 13);
}

#[test]
fn retransmit_buffer_bound_faults_oldest() {
  let pair = Pair::new(no_timer().with_max_unacked(2));
  let failed = failures(&pair.a);
  let pid = pair.spawn_counter();

  pair.a_to_b.set_up(false);
  for x in [1, 10, 100] {
    run(pair.a.cast_reliable(&pid, add(x))).expect("buffered");
  }
  assert_eq!(pair.a.unacked("b@test"), 2);
  {
    let failed = failed.lock().expect("failures");
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].seq, 1, "the oldest message made room");
    assert_eq!(failed[0].to, pid);
    assert_eq!(failed[0].peer, "b@test");
    assert_eq!(failed[0].reason, DeliveryFailure::BufferFull);
  }

  // Only the two kept messages arrive; b's window slides past the faulted one.
  pair.reconnect();
  assert_eq!(pair.count(&pid), 110);
  run(pair.a.cast_reliable(&pid, add(1000))).expect("sent");
  assert_eq!(pair.count(&pid), 1110);

  // The byte bound refuses a message larger than the whole buffer.
  let tiny = Pair::new(no_timer().with_max_unacked_bytes(4));
  let pid = tiny.spawn_counter();
  assert_eq!(
    run(tiny.a.cast_reliable(&pid, vec![0; 5])),
    Err(RemoteFault::MailboxFull)
  );
}

#[test]
fn pending_messages_fail_when_peer_restarts() {
  let pair = Pair::new(no_timer());
  let failed = failures(&pair.a);
  let pid = pair.spawn_counter();
  pair.a_to_b.set_up(false);
  run(pair.a.cast_reliable(&pid, add(1))).expect("buffered");

  // b comes back as a new incarnation: its old processes are gone, so nothing is resent.
  let reborn = NodeId::from_parts("b@test", 2);
  let resent = pair.a_to_b.reliable.load(Ordering::SeqCst);
  pair.a_to_b.set_up(true);
  pair.a.add_peer_node(
    &reborn,
    pair.a_to_b.sender(pair.a_id.clone(), pair.b.clone()),
  );
  assert_eq!(pair.a_to_b.reliable.load(Ordering::SeqCst), resent);
  assert_eq!(pair.a.unacked("b@test"), 0);
  let failed = failed.lock().expect("failures");
  assert_eq!(failed.len(), 1);
  assert_eq!(failed[0].reason, DeliveryFailure::PeerRestarted);
  assert_eq!(failed[0].to, pid);
}

#[test]
fn at_most_once_remains_default() {
  assert_eq!(DeliveryConfig::default().mode, DeliveryMode::AtMostOnce);
  let pair = Pair::new(DeliveryConfig::default());
  assert_eq!(pair.a.delivery().mode, DeliveryMode::AtMostOnce);
  let pid = pair.spawn_counter();

  // A drop loses the message, as ADR 0009 says; nothing is buffered or resent.
  pair.a_to_b.set_up(false);
  assert_eq!(pair.a.remote_cast(&pid, add(1)), Err(RemoteFault::NoProc));
  assert_eq!(run(pair.a.cast(&pid, add(1))), Err(RemoteFault::NoProc));
  assert_eq!(pair.a.unacked("b@test"), 0);
  pair.reconnect();
  assert_eq!(pair.count(&pid), 0);

  // While connected, plain sends travel as plain frames.
  pair.a.remote_cast(&pid, add(2)).expect("cast");
  run(
    pair
      .a
      .send(&pid, postcard::to_allocvec(&3u32).expect("info")),
  )
  .expect("send");
  assert_eq!(pair.count(&pid), 5);
  assert_eq!(pair.a_to_b.reliable.load(Ordering::SeqCst), 0);

  // Per-mesh opt-in makes the same calls reliable.
  let reliable = Pair::new(no_timer());
  let pid = reliable.spawn_counter();
  reliable.a_to_b.set_up(false);
  reliable.a.remote_cast(&pid, add(4)).expect("buffered");
  reliable.reconnect();
  assert_eq!(reliable.count(&pid), 4);
}

#[test]
fn addr_send_reliable_delivers_to_a_local_mailbox() {
  let a = mesh(&NodeId::from_parts("a@test", 1));
  let addr = a.process().spawn::<Counter>((), ());
  let sent: Effect<(), RemoteFault> = addr.send_reliable(&a, 9);
  run(sent).expect("send");
  let call = addr.call::<id_effect::HNil>(CounterCall::Get, WAIT);
  assert!(matches!(call.block_on(), Exit::Success(9)));
}

/// A recorded span: name and attributes.
type Recorded = (String, Vec<(String, String)>);

/// Records every span the receiving node opens.
#[derive(Default)]
struct Recorder {
  spans: Mutex<Vec<Recorded>>,
}

impl SpanHook for Recorder {
  fn on_start(&self, span: &Span) {
    self
      .spans
      .lock()
      .expect("spans")
      .push((span.name().to_string(), span.attributes().to_vec()));
  }

  fn on_end(&self, _: &Span, _: Instant, _: ExitKind) {}
}

impl Recorder {
  /// The `mesh.parent` attribute of the first span named `name`.
  fn parent_of(&self, name: &str) -> Option<String> {
    let spans = self.spans.lock().expect("spans");
    let (_, attributes) = spans.iter().find(|(n, _)| n == name)?;
    attributes
      .iter()
      .find(|(k, _)| k == "mesh.parent")
      .map(|(_, v)| v.clone())
  }

  fn attribute(&self, name: &str, key: &str) -> Option<String> {
    let spans = self.spans.lock().expect("spans");
    let (_, attributes) = spans.iter().find(|(n, _)| n == name)?;
    attributes
      .iter()
      .find(|(k, _)| k == key)
      .map(|(_, v)| v.clone())
  }
}

/// Runs `send` inside a span named `name` and returns that span's `traceparent`.
fn traced(name: &str, send: Effect<(), RemoteFault>) -> String {
  let eff: Effect<String, RemoteFault> = Span::current()
    .flat_map(move |span| {
      let parent = traceparent_of(&span.expect("inside a span"));
      send.map(move |()| parent)
    })
    .with_span(name.to_string());
  run(eff).expect("traced send")
}

#[test]
fn remote_call_carries_trace_context() {
  let recorder = Arc::new(Recorder::default());
  let hook: Arc<dyn SpanHook> = recorder.clone();
  let pair = Pair::with(DeliveryConfig::default(), |b| {
    b.with_trace_propagator(Arc::new(W3cTraceContext::new().with_receiver_hook(hook)))
  });
  let pid = pair.spawn_counter();
  let get = postcard::to_allocvec(&CounterCall::Get).expect("encode");

  let (a, p) = (pair.a.clone(), pid.clone());
  let call_parent = traced("client.call", a.remote_call(&p, get, WAIT).map(|_reply| ()));
  assert!(eventually(|| recorder.parent_of("mesh.call").is_some()));
  assert_eq!(recorder.parent_of("mesh.call"), Some(call_parent));
  assert_eq!(
    recorder.attribute("mesh.call", "otel.kind").as_deref(),
    Some("server")
  );
  assert_eq!(
    recorder.attribute("mesh.call", "mesh.from").as_deref(),
    Some("a@test")
  );

  // Casts and sends carry it too.
  let cast_parent = traced("client.cast", pair.a.cast(&pid, add(1)));
  let info = postcard::to_allocvec(&1u32).expect("info");
  let send_parent = traced("client.send", pair.a.send_reliable(&pid, info));
  assert!(eventually(
    || recorder.parent_of("mesh.cast").is_some() && recorder.parent_of("mesh.info").is_some()
  ));
  assert_eq!(recorder.parent_of("mesh.cast"), Some(cast_parent));
  assert_eq!(recorder.parent_of("mesh.info"), Some(send_parent));
  assert_eq!(
    recorder.attribute("mesh.cast", "otel.kind").as_deref(),
    Some("consumer")
  );

  // Outside any span nothing is carried, and the receiver opens no span.
  let before = recorder.spans.lock().expect("spans").len();
  pair.a.remote_cast(&pid, add(1)).expect("untraced");
  assert_eq!(pair.count(&pid), 3, "cast, send and this cast all arrived");
  std::thread::sleep(Duration::from_millis(50));
  assert_eq!(recorder.spans.lock().expect("spans").len(), before);
}

const SECRET: &str = "a-cluster-cookie-of-at-least-32-bytes";

#[tokio::test]
async fn version_mismatch_refused() {
  assert_eq!(PROTOCOL_VERSION, 3);
  let cookie = Cookie::try_new(SECRET).expect("cookie");
  let net = MemoryNetwork::new();
  let listener = net.transport("b").listen("b").await.expect("bind");

  // A 0.10.1 (protocol v2) node dialing in is refused, and told why.
  let old = net.transport("a").connect("b").await.expect("dial");
  let conn = listener.accept().await.expect("accept");
  let hello = Frame::Handshake(HandshakeMsg::Hello {
    version: 2,
    node: NodeId::from_parts("a@old", 1),
  });
  old
    .send(encode(&hello).expect("encode"))
    .await
    .expect("send");
  let err = Session::respond(conn, &NodeId::from_parts("b@test", 2), &cookie)
    .await
    .err()
    .expect("refused");
  assert!(matches!(
    err,
    SessionError::Handshake(HandshakeError::VersionMismatch { ours: 3, theirs: 2 })
  ));
  assert_eq!(
    decode(&old.recv().await.expect("refusal")).expect("decode"),
    Frame::Handshake(HandshakeMsg::Rejected {
      reason: "version mismatch: have 3, got 2".to_string()
    })
  );

  // Dialing a v2 node fails the same way when it answers with its version.
  let dial = net.transport("c").connect("b").await.expect("dial");
  let old_side = listener.accept().await.expect("accept");
  let me = NodeId::from_parts("c@test", 3);
  let (initiated, ()) = tokio::join!(Session::initiate(dial, &me, &cookie), async {
    let _hello = old_side.recv().await.expect("hello");
    let challenge = Frame::Handshake(HandshakeMsg::Challenge {
      version: 2,
      node: NodeId::from_parts("b@old", 1),
      nonce: 7,
    });
    old_side
      .send(encode(&challenge).expect("encode"))
      .await
      .expect("send");
  });
  assert!(matches!(
    initiated.err().expect("refused"),
    SessionError::Handshake(HandshakeError::VersionMismatch { ours: 3, theirs: 2 })
  ));
}
