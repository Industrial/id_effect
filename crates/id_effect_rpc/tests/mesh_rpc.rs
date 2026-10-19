//! RPC over the process mesh (feature `mesh`, r12-e12-2-mesh-rpc): the same `RpcGroup` callable
//! node to node, postcard always, through an endpoint process under a cluster-wide name; every
//! kind with credits; trace context, guards on the verified peer, deadlines, limits, restarts
//! and dead nodes.
#![cfg(all(feature = "mesh", feature = "client"))]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::IntoResponse;
use futures_util::StreamExt as _;
use id_effect::{
  Cause, DynEnv, Effect, Exit, ExitKind, Never, Span, SpanHook, Stream, schema, succeed,
};
use id_effect_axum::route::GuardFuture;
use id_effect_node::handshake::Cookie;
use id_effect_node::transport::Transport as _;
use id_effect_node::transport::memory::MemoryNetwork;
use id_effect_node::{
  BehaviorRegistry, Frame as SessionFrame, Mesh, NodeId, PeerSender, ProcessEnvelope, RemoteFault,
  Session, W3cTraceContext, traceparent_of,
};
use id_effect_process::{ExitReason, NodeName, Pid, ProcessNode, Strategy, SupervisorSpec};
use id_effect_rpc::client::mesh::MeshRpcError;
use id_effect_rpc::kinds::{Duplex, Subscription, SubscriptionItem};
use id_effect_rpc::registry::RouteGuard;
use id_effect_rpc::service::{RpcTransport as _, TransportCall, TransportError};
use id_effect_rpc::transport::mesh::{
  MeshPeer, MeshPeerGuard, MeshRpc, MeshServeError, MeshSetupError, endpoint_name,
};
use id_effect_rpc::wire::{Cursor, Frame, FrameKind, RpcFailure};
use id_effect_rpc::{Procedure, RpcGroup, RpcPrincipal, WireSchema};

const WAIT: Duration = Duration::from_secs(10);

fn int() -> WireSchema<i64> {
  WireSchema::new(schema::i64::<()>()).with_serde()
}

fn text() -> WireSchema<String> {
  WireSchema::new(schema::string::<()>()).with_serde()
}

/// Records the principal and the mesh peer the guards saw.
#[derive(Clone, Default)]
struct Seen(Arc<Mutex<Vec<Sighting>>>);

/// The principal and the mesh peer of one call.
type Sighting = (Option<String>, Option<String>);

impl RouteGuard for Seen {
  fn policy(&self) -> &str {
    "seen"
  }

  fn check<'a>(&'a self, parts: &'a Parts) -> GuardFuture<'a> {
    let principal = parts
      .extensions
      .get::<RpcPrincipal>()
      .map(|RpcPrincipal(p)| p.clone());
    let peer = parts
      .extensions
      .get::<MeshPeer>()
      .map(|MeshPeer(id)| id.to_string());
    self.0.lock().unwrap().push((principal, peer));
    Box::pin(async { Ok(()) })
  }
}

/// An end-user authentication guard: wants a `User` extension no mesh call carries.
struct UserGuard;

#[derive(Clone)]
struct User;

impl RouteGuard for UserGuard {
  fn policy(&self) -> &str {
    "user"
  }

  fn check<'a>(&'a self, parts: &'a Parts) -> GuardFuture<'a> {
    let ok = parts.extensions.get::<User>().is_some();
    Box::pin(async move {
      if ok {
        Ok(())
      } else {
        Err(StatusCode::UNAUTHORIZED.into_response())
      }
    })
  }
}

/// The service every test serves, as `Pc`.
fn group(bumps: &Arc<AtomicUsize>, seen: &Seen) -> RpcGroup {
  let mut group = RpcGroup::new();
  group.query(
    Procedure::query("Pc.get").public(),
    int(),
    text(),
    |n: i64, _: &mut DynEnv| succeed::<_, Never, _>(format!("n{n}")),
  );
  let counter = Arc::clone(bumps);
  group.mutation(
    Procedure::mutation("Pc.bump").public(),
    int(),
    int(),
    move |n: i64, _: &mut DynEnv| {
      counter.fetch_add(1, Ordering::SeqCst);
      succeed::<_, Never, _>(n + 1)
    },
  );
  group.query(
    Procedure::query("Pc.slow").public(),
    int(),
    int(),
    |n: i64, _: &mut DynEnv| {
      id_effect::sleep::<Never, DynEnv>(Duration::from_secs(5)).map(move |()| n)
    },
  );
  group.query(
    Procedure::query("Pc.untyped").public(),
    WireSchema::new(schema::i64::<()>()),
    WireSchema::new(schema::string::<()>()),
    |n: i64, _: &mut DynEnv| succeed::<_, Never, _>(n.to_string()),
  );
  group.query(
    Procedure::query("Pc.peer")
      .guard(MeshPeerGuard::nodes(["a@test"]))
      .guard(seen.clone()),
    int(),
    text(),
    |_: i64, _: &mut DynEnv| succeed::<_, Never, _>("peer".to_owned()),
  );
  group.query(
    Procedure::query("Pc.other").guard(MeshPeerGuard::nodes(["c@test"])),
    int(),
    text(),
    |_: i64, _: &mut DynEnv| succeed::<_, Never, _>("other".to_owned()),
  );
  group.query(
    Procedure::query("Pc.user").guard(UserGuard),
    int(),
    text(),
    |_: i64, _: &mut DynEnv| succeed::<_, Never, _>("user".to_owned()),
  );
  group.server_stream(
    Procedure::server_stream("Pc.count").public(),
    int(),
    int(),
    |n: i64, _: &mut DynEnv| succeed::<_, Never, _>(Stream::from_iterable(1..=n)),
  );
  group.server_stream(
    Procedure::server_stream("Pc.forever").public(),
    int(),
    int(),
    |n: i64, _: &mut DynEnv| succeed::<_, Never, _>(Stream::from_iterable(n..)),
  );
  group.client_stream(
    Procedure::client_stream("Pc.sum").public().max_items(150),
    int(),
    int(),
    |items: Stream<i64, Never, DynEnv>, _: &mut DynEnv| items.run_fold(0, |a, b| a + b),
  );
  group.bidi(
    Procedure::bidi("Pc.shout").public(),
    text(),
    text(),
    |_: &mut DynEnv| {
      succeed::<_, Never, _>(Duplex::new(|items: Stream<String, Never, DynEnv>| {
        items.map(|s| s.to_uppercase())
      }))
    },
  );
  // An output schema that refuses every value it produced: the encode bridge fails.
  let broken = || {
    WireSchema::from_fns(
      |_: &serde_json::Value| Err(Vec::new()),
      |n: i64| Ok(serde_json::json!(n)),
      serde_json::json!({}),
    )
    .with_serde()
  };
  group.query(
    Procedure::query("Pc.broken").public(),
    int(),
    broken(),
    |n: i64, _: &mut DynEnv| succeed::<_, Never, _>(n),
  );
  group.server_stream(
    Procedure::server_stream("Pc.brokenStream").public(),
    int(),
    broken(),
    |n: i64, _: &mut DynEnv| succeed::<_, Never, _>(Stream::from_iterable(0..n)),
  );
  group.client_stream(
    Procedure::client_stream("Pc.ignore").public(),
    int(),
    int(),
    |_items: Stream<i64, Never, DynEnv>, _: &mut DynEnv| succeed::<_, Never, _>(0_i64),
  );
  group.client_stream(
    Procedure::client_stream("Pc.small").public().max_bytes(8),
    int(),
    int(),
    |items: Stream<i64, Never, DynEnv>, _: &mut DynEnv| items.run_fold(0, |a, b| a + b),
  );
  group.bidi(
    Procedure::bidi("Pc.once").public(),
    int(),
    int(),
    |_: &mut DynEnv| {
      succeed::<_, Never, _>(Duplex::new(|_items: Stream<i64, Never, DynEnv>| {
        Stream::from_iterable(1_i64..=1)
      }))
    },
  );
  group.subscription(
    Procedure::subscription("Pc.feed").public(),
    text(),
    text(),
    |prefix: String, after: Option<Cursor>, _: &mut DynEnv| {
      let first = u64::from(after.is_some()) + 1;
      let items = (first..=2_u64)
        .map(|n| SubscriptionItem::new(format!("{prefix}{n}")).with_cursor(Cursor::events(n)))
        .collect();
      succeed::<_, Never, _>(Subscription::new(Stream::from_vec(items)))
    },
  );
  group
}

struct TestNode {
  id: NodeId,
  rpc: MeshRpc,
}

fn registry() -> BehaviorRegistry {
  let mut registry = BehaviorRegistry::new();
  MeshRpc::register_behavior(&mut registry);
  registry
}

fn mesh(id: &NodeId, registry: BehaviorRegistry) -> Mesh {
  Mesh::new(
    id.clone(),
    ProcessNode::new(NodeName::new(id.name())),
    registry,
  )
}

fn node(name: &str, incarnation: u128) -> TestNode {
  let id = NodeId::from_parts(name, incarnation);
  let rpc = MeshRpc::new(id.clone(), mesh(&id, registry())).expect("mesh rpc");
  TestNode { id, rpc }
}

fn gated(up: &Arc<AtomicBool>, inner: PeerSender) -> PeerSender {
  let up = Arc::clone(up);
  Arc::new(move |envelope| {
    if up.load(Ordering::SeqCst) {
      inner(envelope)
    } else {
      Err(RemoteFault::NoProc)
    }
  })
}

/// `a` and `b` joined through each other's ingress; the returned switches gate a→b and b→a.
fn connect(a: &TestNode, b: &TestNode) -> (Arc<AtomicBool>, Arc<AtomicBool>) {
  let (ab, ba) = (
    Arc::new(AtomicBool::new(true)),
    Arc::new(AtomicBool::new(true)),
  );
  a.rpc
    .mesh()
    .add_peer_node(&b.id, gated(&ab, b.rpc.peer_sender(a.id.clone())));
  b.rpc
    .mesh()
    .add_peer_node(&a.id, gated(&ba, a.rpc.peer_sender(b.id.clone())));
  (ab, ba)
}

struct Cluster {
  a: TestNode,
  b: TestNode,
  ba: Arc<AtomicBool>,
  bumps: Arc<AtomicUsize>,
  seen: Seen,
}

fn cluster() -> Cluster {
  let (a, b) = (node("a@test", 1), node("b@test", 2));
  let (_ab, ba) = connect(&a, &b);
  Cluster {
    a,
    b,
    ba,
    bumps: Arc::default(),
    seen: Seen::default(),
  }
}

impl Cluster {
  fn serve(&self) -> id_effect_rpc::transport::mesh::MeshEndpoint {
    self
      .b
      .rpc
      .serve("Pc", group(&self.bumps, &self.seen), DynEnv::new())
  }

  fn endpoint_on_b(&self) -> Option<Pid> {
    self.b.rpc.mesh().global().whereis(&endpoint_name("Pc"))
  }
}

async fn run<A: Send + 'static>(effect: Effect<A, MeshRpcError, ()>) -> Result<A, MeshRpcError> {
  match id_effect_axum::run_with_env((), move |_| effect).await {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(cause) => panic!("unexpected cause: {cause:?}"),
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mesh_unary_and_stream_round_trip() {
  let c = cluster();
  let endpoint = c.serve();
  assert_eq!(endpoint.service(), "Pc");
  assert_eq!(endpoint.name(), "rpc:Pc");
  assert!(format!("{endpoint:?}").contains("Pc"));
  let pid = endpoint.start().expect("start");
  assert_eq!(pid.node().as_str(), "b@test");
  assert!(eventually(|| c.a.rpc.mesh().global().whereis("rpc:Pc").is_some()).await);
  let pc = c.a.rpc.client("Pc");
  assert_eq!(pc.service(), "Pc");

  // Unary.
  assert_eq!(
    run(pc.query::<_, String, ()>("Pc.get", &4_i64))
      .await
      .unwrap(),
    "n4"
  );
  assert_eq!(
    run(pc.mutation::<_, i64, ()>("Pc.bump", &2_i64))
      .await
      .unwrap(),
    3
  );

  // A server stream longer than the credit window.
  let stream = run(pc.server_stream::<_, i64, ()>("Pc.count", &50_i64))
    .await
    .unwrap();
  assert!(format!("{stream:?}").contains("MeshStream"));
  let items: Vec<i64> = stream.map(Result::unwrap).collect().await;
  assert_eq!(items, (1..=50).collect::<Vec<_>>());

  // A client stream longer than the credit window.
  let (sink, reply) = run(pc.client_stream::<i64, i64, ()>("Pc.sum"))
    .await
    .unwrap();
  assert!(format!("{sink:?} {reply:?}").contains("MeshSink"));
  for n in 1..=100 {
    sink.send(&n).await.unwrap();
  }
  sink.close().unwrap();
  sink.close().unwrap();
  assert_eq!(reply.recv().await.unwrap(), 5050);
  assert!(sink.send(&1).await.is_err(), "the call is over");

  // Two-way, past the window in both directions.
  let (sink, mut out) = run(pc.bidi::<String, String, ()>("Pc.shout"))
    .await
    .unwrap();
  for n in 0..80 {
    sink.send(&format!("w{n}")).await.unwrap();
    assert_eq!(out.recv().await.unwrap().unwrap(), format!("W{n}"));
  }
  drop(sink);
  assert!(out.recv().await.is_none(), "ends after the input ends");
  assert!(out.recv().await.is_none());

  // A subscription, resumed after a cursor.
  let mut feed = run(pc.subscribe::<_, String, ()>("Pc.feed", "e", None))
    .await
    .unwrap();
  assert_eq!(feed.recv().await.unwrap().unwrap(), "e1");
  assert_eq!(feed.cursor(), Some(Cursor::events(1)));
  assert_eq!(feed.recv().await.unwrap().unwrap(), "e2");
  assert!(feed.recv().await.is_none());
  assert_eq!(feed.cursor(), Some(Cursor::events(2)));
  let mut resumed = run(pc.subscribe::<_, String, ()>("Pc.feed", "e", Some(Cursor::events(1))))
    .await
    .unwrap();
  assert_eq!(resumed.recv().await.unwrap().unwrap(), "e2");

  // Refusals are typed: unknown procedure, no typed codec, wrong input type.
  let unknown = run(pc.query::<_, String, ()>("Pc.nope", &1_i64))
    .await
    .unwrap_err();
  assert_eq!(unknown.rejected_status(), Some(404));
  assert_eq!(unknown.failure_tag(), Some("Rejected"));
  let untyped = run(pc.query::<_, String, ()>("Pc.untyped", &1_i64))
    .await
    .unwrap_err();
  assert_eq!(untyped.rejected_status(), Some(415));
  let invalid = run(pc.query::<_, String, ()>("Pc.get", "not a number"))
    .await
    .unwrap_err();
  assert!(matches!(
    invalid.reserved(),
    Some(RpcFailure::InvalidInput { .. })
  ));
  let wrong_output = run(pc.query::<_, i64, ()>("Pc.get", &1_i64))
    .await
    .unwrap_err();
  assert!(matches!(wrong_output, MeshRpcError::Codec(_)));

  // The endpoint's own node calls it too (a local delivery).
  let local = c.b.rpc.client("Pc");
  assert_eq!(
    run(local.query::<_, String, ()>("Pc.get", &1_i64))
      .await
      .unwrap(),
    "n1"
  );
  let (sink, mut out) = run(local.bidi::<String, String, ()>("Pc.shout"))
    .await
    .unwrap();
  sink.send(&"x".to_owned()).await.unwrap();
  assert_eq!(out.recv().await.unwrap().unwrap(), "X");
  drop((sink, out));

  assert!(eventually(|| endpoint.open_calls() == 0).await);
}

/// Records every span the receiving node opens.
#[derive(Default)]
struct Recorder {
  spans: Mutex<Vec<Recorded>>,
}

/// A span's name and attributes.
type Recorded = (String, Vec<(String, String)>);

impl SpanHook for Recorder {
  fn on_start(&self, span: &Span) {
    self
      .spans
      .lock()
      .unwrap()
      .push((span.name().to_owned(), span.attributes().to_vec()));
  }

  fn on_end(&self, _: &Span, _: Instant, _: ExitKind) {}
}

impl Recorder {
  fn attribute(&self, name: &str, key: &str) -> Option<String> {
    let spans = self.spans.lock().unwrap();
    let (_, attributes) = spans.iter().find(|(n, _)| n == name)?;
    attributes
      .iter()
      .find(|(k, _)| k == key)
      .map(|(_, v)| v.clone())
  }

  fn count(&self, name: &str) -> usize {
    self
      .spans
      .lock()
      .unwrap()
      .iter()
      .filter(|(n, _)| n == name)
      .count()
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mesh_call_carries_trace_context() {
  let c = cluster();
  let recorder = Arc::new(Recorder::default());
  let hook: Arc<dyn SpanHook> = recorder.clone();
  let _ = c
    .b
    .rpc
    .clone()
    .with_trace_propagator(Arc::new(W3cTraceContext::new().with_receiver_hook(hook)));
  c.serve().start().expect("start");
  let pc = c.a.rpc.client("Pc");

  let call = pc.query::<_, String, ()>("Pc.get", &7_i64);
  let traced: Effect<(String, String), MeshRpcError, ()> = Span::current()
    .flat_map(move |span| {
      let parent = traceparent_of(&span.expect("inside a span"));
      call.map(move |out| (parent, out))
    })
    .with_span("client.call");
  let (parent, out) = run(traced).await.unwrap();
  assert_eq!(out, "n7");
  assert!(eventually(|| recorder.attribute("rpc.request", "mesh.parent").is_some()).await);
  assert_eq!(
    recorder.attribute("rpc.request", "mesh.parent"),
    Some(parent)
  );
  for (key, value) in [
    ("otel.kind", "server"),
    ("rpc.system", "id_effect_rpc"),
    ("rpc.service", "Pc"),
    ("rpc.method", "get"),
    ("rpc.procedure", "Pc.get"),
    ("mesh.node", "b@test"),
    ("mesh.from", "a@test"),
  ] {
    assert_eq!(
      recorder.attribute("rpc.request", key).as_deref(),
      Some(value),
      "{key}"
    );
  }

  // Outside any span nothing is carried and no receiving span opens.
  let before = recorder.count("rpc.request");
  assert_eq!(
    run(pc.query::<_, String, ()>("Pc.get", &8_i64))
      .await
      .unwrap(),
    "n8"
  );
  tokio::time::sleep(Duration::from_millis(50)).await;
  assert_eq!(recorder.count("rpc.request"), before);
}

/// `b`'s endpoint under a supervisor; its name visible on `a`.
async fn supervised(c: &Cluster) -> Pid {
  let endpoint = c.serve();
  let spec = SupervisorSpec::new(Strategy::OneForOne)
    .intensity(100, Duration::from_secs(1))
    .child(endpoint.child_spec());
  let _supervisor = spec.start(c.b.rpc.mesh().process());
  assert!(eventually(|| c.a.rpc.mesh().global().whereis("rpc:Pc").is_some()).await);
  c.endpoint_on_b().unwrap()
}

/// Kill `b`'s endpoint while `a` does not hear of it: `a` keeps the old pid while the supervisor
/// restarts the endpoint under the name. Returns the old pid; `b → a` is up again.
async fn stale_view(c: &Cluster) -> Pid {
  let old = c.endpoint_on_b().unwrap();
  c.ba.store(false, Ordering::SeqCst);
  c.b.rpc.mesh().process().exit(&old, ExitReason::Killed);
  assert!(eventually(|| c.endpoint_on_b().is_some_and(|p| p != old)).await);
  c.ba.store(true, Ordering::SeqCst);
  assert_eq!(c.a.rpc.mesh().global().whereis("rpc:Pc"), Some(old.clone()));
  old
}

/// `b` tells `a` its names after `delay`.
fn sync_later(c: &Cluster, delay: Duration) {
  let mesh = c.b.rpc.mesh().clone();
  tokio::spawn(async move {
    tokio::time::sleep(delay).await;
    mesh.global().sync_with("a@test").expect("sync");
  });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mesh_endpoint_restart_is_transparent_to_callers() {
  let c = cluster();
  let first = supervised(&c).await;
  let pc = c.a.rpc.client("Pc");
  assert_eq!(
    run(pc.query::<_, String, ()>("Pc.get", &1_i64))
      .await
      .unwrap(),
    "n1"
  );

  // Killed, then called at once: re-resolved until the restarted endpoint holds the name.
  c.b.rpc.mesh().process().exit(&first, ExitReason::Killed);
  assert_eq!(
    run(pc.query::<_, String, ()>("Pc.get", &2_i64))
      .await
      .unwrap(),
    "n2"
  );
  let second = c.endpoint_on_b().unwrap();
  assert_ne!(second, first);

  // A stale pid (noproc) is re-resolved too.
  let stale = stale_view(&c).await;
  sync_later(&c, Duration::from_millis(20));
  assert_eq!(
    run(pc.query::<_, String, ()>("Pc.get", &3_i64))
      .await
      .unwrap(),
    "n3"
  );
  assert_ne!(c.a.rpc.mesh().global().whereis("rpc:Pc"), Some(stale));

  // A stream whose endpoint is killed ends with a typed failure, never a hang.
  let mut forever = run(pc.server_stream::<_, i64, ()>("Pc.forever", &0_i64))
    .await
    .unwrap();
  assert_eq!(forever.recv().await.unwrap().unwrap(), 0);
  let current = c.endpoint_on_b().unwrap();
  c.b.rpc.mesh().process().exit(&current, ExitReason::Killed);
  let error = loop {
    match forever.recv().await.expect("an item or the failure") {
      Ok(_) => {}
      Err(error) => break error,
    }
  };
  assert!(matches!(&error, MeshRpcError::EndpointDown(why) if why.contains("Killed")));
  assert!(error.is_endpoint_gone());
  assert!(forever.recv().await.is_none());

  // Queries are retried a bounded number of times only.
  let none = c
    .a
    .rpc
    .client("Nobody")
    .with_query_retries(1)
    .with_retry_backoff(Duration::from_millis(1));
  let error = run(none.query::<_, String, ()>("Nobody.get", &1_i64))
    .await
    .unwrap_err();
  assert_eq!(error, MeshRpcError::NoEndpoint("Nobody".to_owned()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mesh_mutation_not_retried_on_noproc() {
  let c = cluster();
  supervised(&c).await;
  let pc = c.a.rpc.client("Pc");
  stale_view(&c).await;
  // Were the mutation retried (backoff 50 ms), it would find the endpoint the sync announces.
  sync_later(&c, Duration::from_millis(20));
  let error = run(pc.mutation::<_, i64, ()>("Pc.bump", &1_i64))
    .await
    .unwrap_err();
  assert_eq!(error, MeshRpcError::NoEndpoint("Pc".to_owned()));
  assert!(error.to_string().contains("Pc"));
  assert_eq!(c.bumps.load(Ordering::SeqCst), 0, "the mutation never ran");
  assert!(eventually(|| c.a.rpc.mesh().global().whereis("rpc:Pc") == c.endpoint_on_b()).await);
  assert_eq!(
    run(pc.mutation::<_, i64, ()>("Pc.bump", &1_i64))
      .await
      .unwrap(),
    2
  );
  assert_eq!(c.bumps.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mesh_calls_bypassing_the_ingress_are_dropped() {
  let (a, b) = (node("a@test", 1), node("b@test", 2));
  // a → b straight into the mesh: no ingress, so no verified origin.
  let b_mesh = b.rpc.mesh().clone();
  let a_id = a.id.clone();
  a.rpc.mesh().add_peer_node(
    &b.id,
    Arc::new(move |envelope| b_mesh.handle_envelope_from(&a_id, envelope)),
  );
  b.rpc
    .mesh()
    .add_peer_node(&a.id, a.rpc.peer_sender(b.id.clone()));
  let bumps = Arc::new(AtomicUsize::new(0));
  b.rpc
    .serve("Pc", group(&bumps, &Seen::default()), DynEnv::new())
    .start()
    .expect("start");
  assert!(eventually(|| a.rpc.mesh().global().whereis("rpc:Pc").is_some()).await);
  let pc = a.rpc.client("Pc").with_timeout(Duration::from_millis(300));
  let error = run(pc.mutation::<_, i64, ()>("Pc.bump", &1_i64))
    .await
    .unwrap_err();
  assert_eq!(error.reserved(), Some(RpcFailure::DeadlineExceeded));
  assert_eq!(bumps.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mesh_guards_see_the_calling_node_not_its_users() {
  let c = cluster();
  c.serve().start().expect("start");
  let pc = c.a.rpc.client("Pc");
  assert_eq!(
    run(pc.query::<_, String, ()>("Pc.peer", &1_i64))
      .await
      .unwrap(),
    "peer"
  );
  let seen = c.seen.0.lock().unwrap().clone();
  assert_eq!(
    seen,
    [(Some("mesh:a@test".to_owned()), Some(c.a.id.to_string()))]
  );
  let other = run(pc.query::<_, String, ()>("Pc.other", &1_i64))
    .await
    .unwrap_err();
  assert_eq!(other.rejected_status(), Some(403));
  let user = run(pc.query::<_, String, ()>("Pc.user", &1_i64))
    .await
    .unwrap_err();
  assert_eq!(user.rejected_status(), Some(401));
  assert_eq!(user.failure_tag(), Some("Rejected"));
  assert_eq!(user.reserved(), None);

  // The guard refuses requests that are not mesh calls.
  let guard = MeshPeerGuard::any_node();
  assert_eq!(guard.policy(), "mesh-peer");
  let mut head = axum::http::Request::new(()).into_parts().0;
  let refused = guard.check(&head).await.unwrap_err();
  assert_eq!(refused.status(), StatusCode::UNAUTHORIZED);
  head
    .extensions
    .insert(MeshPeer(NodeId::from_parts("z@x", 1)));
  assert!(guard.check(&head).await.is_ok());
  let refused = MeshPeerGuard::nodes(["a@test"])
    .check(&head)
    .await
    .unwrap_err();
  assert_eq!(refused.status(), StatusCode::FORBIDDEN);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mesh_limits_and_deadlines() {
  let c = cluster();
  let endpoint = c
    .serve()
    .with_max_calls_per_peer(1)
    .with_max_calls(8)
    .with_max_deadline(Duration::from_millis(200));
  endpoint.start().expect("start");
  let pc = c.a.rpc.client("Pc");

  // One call open per peer: the next is refused before it runs.
  let open = run(pc.server_stream::<_, i64, ()>("Pc.forever", &0_i64))
    .await
    .unwrap();
  assert!(eventually(|| endpoint.open_calls() == 1).await);
  let refused = run(pc.query::<_, String, ()>("Pc.get", &1_i64))
    .await
    .unwrap_err();
  assert!(matches!(
    refused.reserved(),
    Some(RpcFailure::ResourceExhausted { limit }) if limit == "mesh.calls"
  ));
  drop(open);
  assert!(
    eventually(|| endpoint.open_calls() == 0).await,
    "cancelled on drop"
  );
  assert_eq!(
    run(pc.query::<_, String, ()>("Pc.get", &1_i64))
      .await
      .unwrap(),
    "n1"
  );

  // The endpoint's deadline cap.
  let slow = run(pc.query::<_, i64, ()>("Pc.slow", &1_i64))
    .await
    .unwrap_err();
  assert_eq!(slow.reserved(), Some(RpcFailure::DeadlineExceeded));

  // The client's timeout: the call is cancelled on the endpoint too.
  let quick = pc.clone().with_timeout(Duration::from_millis(50));
  let slow = run(quick.query::<_, i64, ()>("Pc.slow", &1_i64))
    .await
    .unwrap_err();
  assert_eq!(slow.reserved(), Some(RpcFailure::DeadlineExceeded));
  assert!(eventually(|| endpoint.open_calls() == 0).await);

  // A client stream over its item cap.
  let (sink, reply) = run(pc.client_stream::<i64, i64, ()>("Pc.sum"))
    .await
    .unwrap();
  let feeding = tokio::spawn(async move {
    for n in 0..200 {
      if sink.send(&n).await.is_err() {
        break;
      }
    }
  });
  let over = reply.recv().await.unwrap_err();
  assert!(matches!(
    over.reserved(),
    Some(RpcFailure::ResourceExhausted { limit }) if limit == "items"
  ));
  feeding.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mesh_node_down_fails_calls_with_a_typed_error() {
  let c = cluster();
  let endpoint = c.serve();
  endpoint.start().expect("start");
  let pc = c.a.rpc.client("Pc").with_query_retries(0);

  // The endpoint's node hears the caller's node is down: the call is dropped there.
  let caller_side = run(pc.server_stream::<_, i64, ()>("Pc.forever", &0_i64))
    .await
    .unwrap();
  assert!(eventually(|| endpoint.open_calls() == 1).await);
  c.b.rpc.mesh().peer_down("a@test");
  assert!(eventually(|| endpoint.open_calls() == 0).await);
  drop(caller_side);
  c.b
    .rpc
    .mesh()
    .add_peer_node(&c.a.id, c.a.rpc.peer_sender(c.b.id.clone()));

  let mut forever = run(pc.server_stream::<_, i64, ()>("Pc.forever", &0_i64))
    .await
    .unwrap();
  assert_eq!(forever.recv().await.unwrap().unwrap(), 0);
  c.a.rpc.mesh().peer_down("b@test");
  let error = loop {
    match forever.recv().await.expect("an item or the failure") {
      Ok(_) => {}
      Err(error) => break error,
    }
  };
  assert_eq!(error, MeshRpcError::NodeDown);
  let gone = run(pc.query::<_, String, ()>("Pc.get", &1_i64))
    .await
    .unwrap_err();
  assert!(gone.is_endpoint_gone());
}

/// A value whose serialization always fails.
struct Unserializable;

impl serde::Serialize for Unserializable {
  fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
    Err(serde::ser::Error::custom("no"))
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mesh_edge_cases_fail_typed() {
  let c = cluster();
  let endpoint = c.serve();
  endpoint.start().expect("start");
  let pc = c.a.rpc.client("Pc");
  let codec = |e: MeshRpcError| matches!(e, MeshRpcError::Codec(_));
  let invalid = |e: &MeshRpcError| matches!(e.reserved(), Some(RpcFailure::InvalidInput { .. }));
  let internal = |e: &MeshRpcError| matches!(e.reserved(), Some(RpcFailure::InternalError { .. }));

  // Inputs that do not encode never leave the node.
  assert!(codec(
    run(pc.query::<_, i64, ()>("Pc.get", &Unserializable))
      .await
      .unwrap_err()
  ));
  assert!(codec(
    run(pc.mutation::<_, i64, ()>("Pc.bump", &Unserializable))
      .await
      .unwrap_err()
  ));
  assert!(codec(
    run(pc.server_stream::<_, i64, ()>("Pc.count", &Unserializable))
      .await
      .unwrap_err()
  ));
  let (sink, reply) = run(pc.client_stream::<Unserializable, i64, ()>("Pc.sum"))
    .await
    .unwrap();
  assert!(codec(sink.send(&Unserializable).await.unwrap_err()));
  drop((sink, reply));

  // Outputs the endpoint cannot encode are `InternalError`, for a value and for an item.
  assert!(internal(
    &run(pc.query::<_, i64, ()>("Pc.broken", &1_i64))
      .await
      .unwrap_err()
  ));
  let mut items = run(pc.server_stream::<_, i64, ()>("Pc.brokenStream", &3_i64))
    .await
    .unwrap();
  assert!(internal(&items.recv().await.unwrap().unwrap_err()));
  assert!(items.recv().await.is_none());

  // Inputs of the wrong type: the call input of a stream, an item of a two-way stream.
  let mut items = run(pc.server_stream::<_, i64, ()>("Pc.count", "x"))
    .await
    .unwrap();
  assert!(invalid(&items.recv().await.unwrap().unwrap_err()));
  let mut feed = run(pc.subscribe::<_, String, ()>("Pc.feed", &1_i64, None))
    .await
    .unwrap();
  assert!(invalid(&feed.recv().await.unwrap().unwrap_err()));
  let (sink, mut out) = run(pc.bidi::<i64, String, ()>("Pc.shout")).await.unwrap();
  sink.send(&5).await.unwrap();
  assert!(invalid(&out.recv().await.unwrap().unwrap_err()));
  drop(sink);

  // A handler done before its input: the call ends, the input is left.
  let (sink, reply) = run(pc.client_stream::<i64, i64, ()>("Pc.ignore"))
    .await
    .unwrap();
  sink.send(&1).await.unwrap();
  assert_eq!(reply.recv().await.unwrap(), 0);
  let (sink, mut out) = run(pc.bidi::<i64, i64, ()>("Pc.once")).await.unwrap();
  assert_eq!(out.recv().await.unwrap().unwrap(), 1);
  assert!(out.recv().await.is_none());
  drop(sink);

  // A client stream over its byte cap.
  let (sink, reply) = run(pc.client_stream::<i64, i64, ()>("Pc.small"))
    .await
    .unwrap();
  let feeding = tokio::spawn(async move {
    for n in 0..64 {
      if sink.send(&n).await.is_err() {
        break;
      }
    }
  });
  assert!(matches!(
    reply.recv().await.unwrap_err().reserved(),
    Some(RpcFailure::ResourceExhausted { limit }) if limit == "bytes"
  ));
  feeding.await.unwrap();
  assert!(eventually(|| endpoint.open_calls() == 0).await);
}

async fn session_pair(a: &NodeId, b: &NodeId) -> (Session, Session) {
  let net = MemoryNetwork::new();
  let listener = net.transport("b").listen("b").await.expect("bind");
  let dial = net.transport("a").connect("b").await.expect("dial");
  let accept = listener.accept().await.expect("accept");
  let cookie = Cookie::new("mesh-rpc-cookie-of-at-least-32-bytes");
  let (a, b) = tokio::join!(
    Session::initiate(dial, a, &cookie),
    Session::respond(accept, b, &cookie),
  );
  (a.expect("a session"), b.expect("b session"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mesh_rpc_over_an_authenticated_session() {
  let (a, b) = (node("a@test", 1), node("b@test", 2));
  let (to_b, from_a) = session_pair(&a.id, &b.id).await;
  let (to_b, from_a) = (Arc::new(to_b), Arc::new(from_a));
  // a → b over the session; b pumps it through its ingress.
  let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ProcessEnvelope>();
  a.rpc.mesh().add_peer_node(
    &b.id,
    Arc::new(move |envelope| tx.send(envelope).map_err(|_| RemoteFault::NoProc)),
  );
  let writer = Arc::clone(&to_b);
  tokio::spawn(async move {
    while let Some(envelope) = rx.recv().await {
      Mesh::session_send(&writer, &envelope).await.expect("send");
    }
  });
  let pump = b.rpc.clone();
  let reader = Arc::clone(&from_a);
  let pumped = tokio::spawn(async move {
    loop {
      if let Err(fault) = pump.handle_session_frame(&reader).await {
        return fault;
      }
    }
  });
  b.rpc
    .mesh()
    .add_peer_node(&a.id, a.rpc.peer_sender(b.id.clone()));
  let bumps = Arc::new(AtomicUsize::new(0));
  b.rpc
    .serve("Pc", group(&bumps, &Seen::default()), DynEnv::new())
    .start()
    .expect("start");
  assert!(eventually(|| a.rpc.mesh().global().whereis("rpc:Pc").is_some()).await);
  let pc = a.rpc.client("Pc");
  assert_eq!(
    run(pc.mutation::<_, i64, ()>("Pc.bump", &9_i64))
      .await
      .unwrap(),
    10
  );

  // Control frames are ignored; a close ends the pump.
  to_b.send(&SessionFrame::Ping(1)).await.expect("ping");
  to_b
    .send(&SessionFrame::Close("bye".to_owned()))
    .await
    .expect("close");
  assert_eq!(pumped.await.unwrap(), RemoteFault::NoProc);

  // Garbage data is a decode fault.
  let (to_b, from_a) = session_pair(&a.id, &b.id).await;
  to_b
    .send(&SessionFrame::Data(vec![0xff, 0xff, 0xff]))
    .await
    .expect("send");
  assert!(matches!(
    b.rpc.handle_session_frame(&from_a).await,
    Err(RemoteFault::Decode(_))
  ));
  to_b.close().await;
  assert!(matches!(
    b.rpc.handle_session_frame(&from_a).await,
    Err(RemoteFault::Decode(_))
  ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mesh_rpc_setup_is_checked() {
  let id = NodeId::from_parts("a@test", 1);
  let bare = mesh(&id, BehaviorRegistry::new());
  assert_eq!(
    MeshRpc::new(id.clone(), bare).unwrap_err(),
    MeshSetupError::BehaviorNotRegistered
  );
  let other = NodeId::from_parts("z@test", 1);
  assert_eq!(
    MeshRpc::new(other, mesh(&id, registry())).unwrap_err(),
    MeshSetupError::NodeMismatch
  );
  for error in [
    MeshSetupError::BehaviorNotRegistered,
    MeshSetupError::NoRuntime,
    MeshSetupError::NodeMismatch,
  ] {
    assert!(!error.to_string().is_empty());
  }
  let rpc = MeshRpc::new(id, mesh(&NodeId::from_parts("a@test", 1), registry())).unwrap();
  assert!(format!("{rpc:?}").contains("a@test"));

  // One name, one endpoint: a second node serving the service is refused.
  let c = cluster();
  c.serve().start().expect("start");
  assert!(eventually(|| c.a.rpc.mesh().global().whereis("rpc:Pc").is_some()).await);
  let twin = c.a.rpc.serve(
    "Pc",
    group(&Arc::default(), &Seen::default()),
    DynEnv::new(),
  );
  let refused = twin.start().unwrap_err();
  assert!(matches!(refused, MeshServeError::Name(_)));
  assert!(refused.to_string().contains("rpc:Pc"));
  assert_eq!(twin.open_calls(), 0);
}

// ---------------------------------------------------------------------------------------------
// The same `#[rpc::service]` contract, node to node
// ---------------------------------------------------------------------------------------------

#[derive(Debug, PartialEq, id_effect_rpc::RpcErrorUnion)]
enum RelayError {
  #[rpc(status = 404)]
  NoRoom { room: String },
}

/// Relays chat between nodes.
#[id_effect_rpc::service(version = 1)]
trait Relay {
  /// The text back; an empty one names no room.
  #[query]
  #[public]
  fn echo(&self, text: String) -> Effect<String, RelayError, DynEnv>;

  /// Posts from other nodes only.
  #[mutation]
  #[guard(MeshPeerGuard::any_node())]
  fn post(&self, text: String) -> Effect<i64, Never, DynEnv>;

  /// 1, 2, …, n.
  #[server_stream]
  #[public]
  fn upto(&self, n: i64) -> Effect<Stream<i64, Never, DynEnv>, Never, DynEnv>;

  /// The sum of the numbers.
  #[client_stream]
  #[public]
  fn sum(&self, items: Stream<i64, Never, DynEnv>) -> Effect<i64, Never, DynEnv>;

  /// Every line in upper case.
  #[bidi]
  #[public]
  fn shout(&self) -> Effect<Duplex<String, String, Never>, Never, DynEnv>;

  /// Ticks from `from`, resumable.
  #[subscription]
  #[public]
  fn ticks(
    &self,
    from: i64,
    after: Option<Cursor>,
  ) -> Effect<Subscription<i64, Never>, Never, DynEnv>;
}

#[derive(Default)]
struct Relays {
  posts: Arc<AtomicUsize>,
}

impl Relay for Relays {
  fn echo(&self, text: String) -> Effect<String, RelayError, DynEnv> {
    if text.is_empty() {
      id_effect::fail(RelayError::NoRoom { room: text })
    } else {
      succeed(text)
    }
  }

  fn post(&self, _text: String) -> Effect<i64, Never, DynEnv> {
    let n = self.posts.fetch_add(1, Ordering::SeqCst) + 1;
    succeed(i64::try_from(n).unwrap())
  }

  fn upto(&self, n: i64) -> Effect<Stream<i64, Never, DynEnv>, Never, DynEnv> {
    succeed(Stream::from_iterable(1..=n))
  }

  fn sum(&self, items: Stream<i64, Never, DynEnv>) -> Effect<i64, Never, DynEnv> {
    items.run_fold(0, |a, b| a + b)
  }

  fn shout(&self) -> Effect<Duplex<String, String, Never>, Never, DynEnv> {
    succeed(Duplex::new(|items: Stream<String, Never, DynEnv>| {
      items.map(|s| s.to_uppercase())
    }))
  }

  fn ticks(
    &self,
    from: i64,
    after: Option<Cursor>,
  ) -> Effect<Subscription<i64, Never>, Never, DynEnv> {
    let start = after.map_or(from, |c| i64::try_from(c.position).unwrap() + 1);
    let items = (start..from + 3)
      .map(|n| SubscriptionItem::new(n).with_cursor(Cursor::events(u64::try_from(n).unwrap())))
      .collect();
    succeed(Subscription::new(Stream::from_vec(items)))
  }
}

async fn run_any<A, E>(effect: Effect<A, E, id_effect::HNil>) -> Result<A, E>
where
  A: Send + 'static,
  E: Send + 'static,
{
  id_effect_axum::run_with_env(id_effect::HNil, move |_| effect)
    .await
    .into_result()
    .map_err(|cause| cause.fail_option().expect("a typed failure"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mesh_serves_a_generated_service_to_its_generated_client() {
  use id_effect_rpc::client::mesh::MeshTransport;
  use id_effect_rpc::service::RpcCallError;

  let c = cluster();
  c.b
    .rpc
    .serve(
      "Relay",
      RelayService::group(Relays::default()),
      DynEnv::new(),
    )
    .start()
    .expect("start");
  assert!(eventually(|| c.a.rpc.mesh().global().whereis("rpc:Relay").is_some()).await);
  let transport = MeshTransport::new(
    c.a.rpc.client("Relay"),
    RelayService::group(Relays::default()),
  );
  assert!(format!("{transport:?}").contains("Relay"));
  let relay = RelayService::client(transport);

  assert_eq!(run_any(relay.echo("hi".into())).await.unwrap(), "hi");
  assert_eq!(
    run_any(relay.echo(String::new()))
      .await
      .unwrap_err()
      .typed(),
    Some(&RelayError::NoRoom {
      room: String::new()
    })
  );
  // The guard admits the calling node.
  assert_eq!(run_any(relay.post("x".into())).await.unwrap(), 1);
  let upto = relay.upto(40).flat_map(|items| items.run_collect());
  assert_eq!(run_any(upto).await.unwrap(), (1..=40).collect::<Vec<_>>());
  assert_eq!(run_any(relay.sum((1..=50).collect())).await.unwrap(), 1275);
  let shouted = relay.shout().flat_map(|(tx, out)| {
    tx.send("a".to_owned())
      .flat_map(move |()| tx.send("b".to_owned()).map(move |()| drop(tx)))
      .flat_map(move |()| out.run_collect())
  });
  assert_eq!(run_any(shouted).await.unwrap(), ["A", "B"]);
  let ticks = run_any(relay.ticks(5, None).flat_map(|s| s.run_collect()))
    .await
    .unwrap();
  assert_eq!(ticks.iter().map(|t| t.value).collect::<Vec<_>>(), [5, 6, 7]);
  let resumed = run_any(
    relay
      .ticks(5, ticks[0].cursor)
      .flat_map(|s| s.run_collect()),
  )
  .await
  .unwrap();
  assert_eq!(resumed.iter().map(|t| t.value).collect::<Vec<_>>(), [6, 7]);

  // The transport's own refusals and failures, as a generated client sees them.
  let error = run_any(
    RelayService::client(MeshTransport::new(
      c.a.rpc.client("Nobody"),
      RelayService::group(Relays::default()),
    ))
    .echo("x".into()),
  )
  .await
  .unwrap_err();
  assert!(matches!(
    error,
    RpcCallError::Transport(TransportError::Io(_))
  ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mesh_transport_maps_what_crosses_it() {
  use id_effect_rpc::client::mesh::MeshTransport;
  use id_effect_rpc::contract::ProcedureKind;

  let c = cluster();
  c.serve().start().expect("start");
  assert!(eventually(|| c.a.rpc.mesh().global().whereis("rpc:Pc").is_some()).await);
  let transport = MeshTransport::new(
    c.a.rpc.client("Pc"),
    group(&Arc::default(), &Seen::default()),
  );
  let call = |tag: &str, kind, payload| TransportCall {
    tag: tag.to_owned(),
    version: 1,
    kind,
    payload,
  };
  let refused = |result: Result<_, TransportError>| match result {
    Err(TransportError::Refused { status, .. }) => Some(status),
    _ => None,
  };
  use serde_json::json;
  // Unknown to the schemas (404), no typed codec (415), refused by the endpoint's guard (401).
  let unknown = transport
    .unary(call("Pc.nope", ProcedureKind::Query, json!(1)))
    .await;
  assert_eq!(refused(unknown.map(|_| ())), Some(404));
  let untyped = transport
    .unary(call("Pc.untyped", ProcedureKind::Query, json!(1)))
    .await;
  assert_eq!(refused(untyped.map(|_| ())), Some(415));
  let user = transport
    .unary(call("Pc.user", ProcedureKind::Query, json!(1)))
    .await;
  assert_eq!(refused(user.map(|_| ())), Some(401));
  // An input its codec cannot write.
  let bad = transport
    .unary(call("Pc.get", ProcedureKind::Query, json!("x")))
    .await;
  assert!(matches!(bad, Err(TransportError::Protocol(_))));
  // A failure that is not a refusal is the exit.
  let slow = call("Pc.slow", ProcedureKind::Query, json!(1));
  let quick = MeshTransport::new(
    c.a.rpc.client("Pc").with_timeout(Duration::from_millis(50)),
    group(&Arc::default(), &Seen::default()),
  );
  let exit = quick.unary(slow).await.unwrap();
  assert!(matches!(
    exit,
    id_effect_rpc::wire::WireExit::Failure { .. }
  ));

  // A client stream item its codec cannot write ends the input.
  let (tx, rx) = tokio::sync::mpsc::channel(4);
  tx.send(json!(2)).await.unwrap();
  tx.send(json!("x")).await.unwrap();
  tx.send(json!(5)).await.unwrap();
  let summed = transport
    .client_stream(call("Pc.sum", ProcedureKind::ClientStream, json!(null)), rx)
    .await
    .unwrap();
  assert_eq!(
    summed,
    id_effect_rpc::wire::WireExit::Success { value: json!(2) }
  );

  // Streams: an input that does not encode is refused; a failure is the exit; a refusal and an
  // item that does not decode end the frames without one.
  let bad = transport
    .server_stream(call("Pc.count", ProcedureKind::ServerStream, json!("x")))
    .await;
  assert!(matches!(bad, Err(TransportError::Protocol(_))));
  let frames: Vec<Frame> = transport
    .server_stream(call(
      "Pc.brokenStream",
      ProcedureKind::ServerStream,
      json!(2),
    ))
    .await
    .unwrap()
    .collect()
    .await;
  assert!(
    matches!(&frames[..], [f] if f.kind == FrameKind::Exit),
    "InternalError exit"
  );
  let mut lying = group(&Arc::default(), &Seen::default());
  lying.server_stream(
    Procedure::server_stream("Pc.lies").public(),
    int(),
    text(),
    |_: i64, _: &mut DynEnv| succeed::<_, Never, _>(Stream::from_iterable(std::iter::empty())),
  );
  let mut b_side = group(&Arc::default(), &Seen::default());
  b_side.server_stream(
    Procedure::server_stream("Pc.lies").public(),
    int(),
    int(),
    |n: i64, _: &mut DynEnv| succeed::<_, Never, _>(Stream::from_iterable(n..=n)),
  );
  let d = cluster();
  d.b
    .rpc
    .serve("Pc", b_side, DynEnv::new())
    .start()
    .expect("start");
  assert!(eventually(|| d.a.rpc.mesh().global().whereis("rpc:Pc").is_some()).await);
  let liar = MeshTransport::new(d.a.rpc.client("Pc"), lying);
  let frames: Vec<Frame> = liar
    .server_stream(call("Pc.lies", ProcedureKind::ServerStream, json!(3)))
    .await
    .unwrap()
    .collect()
    .await;
  assert!(frames.is_empty(), "an undecodable item ends the frames");
  let rejected: Vec<Frame> = transport
    .server_stream(call("Pc.user", ProcedureKind::ServerStream, json!(1)))
    .await
    .unwrap()
    .collect()
    .await;
  assert!(rejected.is_empty(), "a refusal ends the frames");
}

#[test]
fn mesh_rpc_needs_a_runtime() {
  let id = NodeId::from_parts("a@test", 1);
  assert_eq!(
    MeshRpc::new(id.clone(), mesh(&id, registry())).unwrap_err(),
    MeshSetupError::NoRuntime
  );
}
