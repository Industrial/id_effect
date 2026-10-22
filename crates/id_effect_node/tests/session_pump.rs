//! F30 (r13-f-mesh-session): the node session pump runs an authenticated session for a mesh,
//! verifying every envelope against the session's peer before the ingress sees it (ADR 0013
//! R1, R8): verified envelopes are delivered, unverified ones dropped and counted.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use id_effect::{Cause, Effect, Exit, Never, Runtime, succeed, sync};
use id_effect_node::handshake::Cookie;
use id_effect_node::session_pump::{DEFAULT_SEND_QUEUE, Ingress, SessionPump, SessionPumpConfig};
use id_effect_node::transport::Transport as _;
use id_effect_node::transport::memory::MemoryNetwork;
use id_effect_node::{
  BehaviorRegistry, ExitReasonWire, Frame, Mesh, NameClaim, NodeId, PidWire, ProcessEnvelope,
  RemoteFault, Session,
};
use id_effect_process::{Next, NodeName, Pid, Process, ProcessCtx, ProcessNode};
use serde::{Deserialize, Serialize};

const WAIT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, Serialize, Deserialize)]
enum CounterCall {
  Get,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
enum CounterCast {
  Add(u32),
}

struct Counter {
  n: u32,
}

impl Process for Counter {
  type Args = ();
  type Env = ();
  type Call = CounterCall;
  type Reply = u32;
  type Cast = CounterCast;
  type Info = ();
  type Error = Never;

  fn init(_args: (), _ctx: ProcessCtx) -> Effect<Self, Never, ()> {
    succeed(Counter { n: 0 })
  }

  fn handle_call(
    self,
    _msg: CounterCall,
    _ctx: ProcessCtx,
  ) -> Effect<(u32, Next<Self>), Never, ()> {
    sync(move || (self.n, Next::Continue(self)))
  }

  fn handle_cast(mut self, msg: CounterCast, _ctx: ProcessCtx) -> Effect<Next<Self>, Never, ()> {
    sync(move || {
      let CounterCast::Add(x) = msg;
      self.n += x;
      Next::Continue(self)
    })
  }
}

fn mesh(name: &str) -> (NodeId, Mesh) {
  let mut registry = BehaviorRegistry::new();
  registry.register::<Counter>("counter");
  let node = NodeName::new(name);
  let id = NodeId::fresh(&node);
  (id.clone(), Mesh::new(id, ProcessNode::new(node), registry))
}

/// An authenticated session pair: `a` dialed `b`.
async fn sessions(a: &NodeId, b: &NodeId) -> (Session, Session) {
  let net = MemoryNetwork::new();
  let listener = net
    .transport(b.name())
    .listen(b.name())
    .await
    .expect("bind");
  let dial = net
    .transport(a.name())
    .connect(b.name())
    .await
    .expect("dial");
  let accepted = listener.accept().await.expect("accept");
  let cookie = Cookie::new("session-pump-cookie-of-at-least-32-bytes");
  let (sa, sb) = tokio::join!(
    Session::initiate(dial, a, &cookie),
    Session::respond(accepted, b, &cookie),
  );
  (sa.expect("a"), sb.expect("b"))
}

/// Run a mesh request on the global runtime.
async fn run<A: Send + 'static>(eff: Effect<A, RemoteFault>) -> Result<A, RemoteFault> {
  match Runtime::global().run(eff).await {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(f)) => Err(f),
    Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
  }
}

async fn eventually(done: impl Fn() -> bool) -> bool {
  let deadline = Instant::now() + WAIT;
  while Instant::now() < deadline {
    if done() {
      return true;
    }
    tokio::time::sleep(Duration::from_millis(5)).await;
  }
  done()
}

async fn count(mesh: &Mesh, pid: &Pid) -> u32 {
  let call = postcard::to_allocvec(&CounterCall::Get).unwrap();
  let reply = run(mesh.remote_call(pid, call, WAIT)).await.expect("call");
  postcard::from_bytes(&reply).unwrap()
}

fn cast(to: &Pid, add: u32) -> ProcessEnvelope {
  ProcessEnvelope::Cast {
    to: PidWire::from(to),
    msg: postcard::to_allocvec(&CounterCast::Add(add)).unwrap(),
    trace: vec![],
  }
}

/// An ingress that counts what it is handed before the mesh dispatches it.
struct Counting {
  mesh: Mesh,
  seen: Arc<AtomicUsize>,
}

impl Ingress for Counting {
  fn mesh(&self) -> &Mesh {
    &self.mesh
  }

  fn deliver(&self, origin: &NodeId, envelope: ProcessEnvelope) -> Result<(), RemoteFault> {
    self.seen.fetch_add(1, Ordering::SeqCst);
    self.mesh.handle_envelope_from(origin, envelope)
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn session_pump_delivers_verified_envelopes_only() {
  let ((a_id, a), (b_id, b)) = (mesh("a@pump"), mesh("b@pump"));
  let (sa, sb) = sessions(&a_id, &b_id).await;
  let seen = Arc::new(AtomicUsize::new(0));
  let pump_a = SessionPump::start(a.clone(), sa);
  let pump_b = SessionPump::start_with(
    Counting {
      mesh: b.clone(),
      seen: Arc::clone(&seen),
    },
    sb,
    SessionPumpConfig::new().with_send_queue(64),
  );
  assert_eq!((pump_a.node(), pump_a.peer().name()), ("a@pump", "b@pump"));
  assert_eq!(pump_b.peer(), &a_id);
  assert!(pump_a.is_open() && pump_b.is_open());
  assert!(format!("{pump_b:?}").contains("b@pump"));

  // Spawn, cast and call across the pumps: every envelope verified, all through the ingress.
  let args = postcard::to_allocvec(&()).unwrap();
  let pid = run(a.remote_spawn("b@pump", "counter", args, None, WAIT))
    .await
    .expect("spawn");
  assert_eq!(pid.node().as_str(), "b@pump");
  a.remote_cast(&pid, postcard::to_allocvec(&CounterCast::Add(4)).unwrap())
    .expect("cast");
  assert_eq!(count(&a, &pid).await, 4);
  assert!(
    eventually(|| pump_b.stats().delivered == 3).await,
    "spawn, cast, call"
  );
  let stats = pump_b.stats();
  assert_eq!((stats.refused, stats.unverified), (0, 0));
  assert_eq!(
    seen.load(Ordering::SeqCst),
    3,
    "everything went through the ingress"
  );
  assert_eq!(pump_a.stats().unverified, 0);

  // A cast to a process that is gone is verified, handed over, and refused by the mesh.
  let gone = Pid::from_parts(NodeName::new("b@pump"), 9_999, pid.incarnation());
  a.remote_cast(&gone, postcard::to_allocvec(&CounterCast::Add(1)).unwrap())
    .expect("queued");
  assert!(eventually(|| pump_b.stats().refused == 1).await);

  // Dropping one side closes the session; the other reports the peer down.
  drop(pump_a);
  assert!(eventually(|| !pump_b.is_open()).await);
  assert!(a.remote_cast(&pid, Vec::new()).is_err(), "a forgot b");
  assert_eq!(
    DEFAULT_SEND_QUEUE,
    SessionPumpConfig::default().send_queue()
  );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn session_pump_drops_unverified_envelope() {
  let ((a_id, _a), (b_id, b)) = (mesh("a@pump"), mesh("b@pump"));
  let (to_b, sb) = sessions(&a_id, &b_id).await;
  let pump = SessionPump::start(b.clone(), sb);
  let pid = b
    .spawn_local("counter", &postcard::to_allocvec(&()).unwrap(), None)
    .expect("spawn");
  let foreign = PidWire::from(&Pid::from_parts(NodeName::new("c@pump"), 1, 1));
  let forged = [
    // For a process of another node.
    ProcessEnvelope::Cast {
      to: foreign.clone(),
      msg: Vec::new(),
      trace: vec![],
    },
    // An exit signal "from" a third node's process: would kill the counter.
    ProcessEnvelope::ExitSignal {
      from: foreign.clone(),
      to: PidWire::from(&pid),
      reason: ExitReasonWire::Killed,
    },
    // A name claim for a third node's process.
    ProcessEnvelope::GlobalRegister {
      claim: NameClaim {
        name: "svc".to_owned(),
        pid: foreign.clone(),
        owner: NodeId::from_parts("c@pump", 1),
      },
    },
  ];
  for envelope in &forged {
    Mesh::session_send(&to_b, envelope).await.expect("send");
  }
  // Interleaved liveness frames are ignored; the verified cast still lands.
  to_b.send(&Frame::Ping(1)).await.expect("ping");
  Mesh::session_send(&to_b, &cast(&pid, 5))
    .await
    .expect("send");
  assert!(eventually(|| pump.stats().unverified == 3 && pump.stats().delivered == 1).await);
  assert_eq!(pump.stats().refused, 0);
  assert_eq!(
    count(&b, &pid).await,
    5,
    "the counter survived the forged exit signal"
  );
  assert!(
    b.global().whereis("svc").is_none(),
    "the forged claim was not merged"
  );

  // An undecodable envelope ends the session.
  to_b
    .send(&Frame::Data(vec![0xff, 0xff, 0xff]))
    .await
    .expect("send");
  assert!(eventually(|| !pump.is_open()).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_pump_ends_on_close() {
  let ((a_id, _a), (b_id, b)) = (mesh("a@pump"), mesh("b@pump"));
  let (to_b, sb) = sessions(&a_id, &b_id).await;
  let pump = SessionPump::start(b.clone(), sb);
  to_b
    .send(&Frame::Close("bye".to_owned()))
    .await
    .expect("close");
  assert!(eventually(|| !pump.is_open()).await);
  // The peer is gone from the mesh: sends to it fail.
  let remote = Pid::from_parts(NodeName::new("a@pump"), 1, 1);
  assert!(b.remote_cast(&remote, Vec::new()).is_err());
}
