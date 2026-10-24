//! F30 and F34 (r13-f-mesh-session): channels and mesh RPC over the node session pump, trace
//! context through the generated client's `MeshTransport`, and mesh streams that fail with a
//! typed `PeerSilent` error when their endpoint's node goes silent (heartbeats, finite
//! deadline), not only on `peer_down`.
#![cfg(all(feature = "mesh", feature = "client"))]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use id_effect::{
  Cause, DynEnv, Effect, Exit, ExitKind, Never, Span, SpanHook, Stream, schema, succeed,
};
use id_effect_node::{
  BehaviorRegistry, Mesh, NodeId, PeerSender, RemoteFault, W3cTraceContext, trace_pair,
  traceparent_of,
};
use id_effect_process::{NodeName, ProcessNode};
use id_effect_rpc::client::mesh::{MeshRpcError, MeshTransport};
use id_effect_rpc::kinds::Duplex;
use id_effect_rpc::resume::TestClock;
use id_effect_rpc::transport::mesh::{
  DEFAULT_HEARTBEAT_INTERVAL, DEFAULT_SILENT_INTERVALS, MeshLiveness, MeshRpc,
};
use id_effect_rpc::{Procedure, RpcGroup, WireSchema};

const WAIT: Duration = Duration::from_secs(10);
const SECOND: Duration = Duration::from_secs(1);

fn int() -> WireSchema<i64> {
  WireSchema::new(schema::i64::<()>()).with_serde()
}

/// The procedures every test serves, as `Lv`.
fn group() -> RpcGroup {
  let mut group = RpcGroup::new();
  group.query(
    Procedure::query("Lv.double").public(),
    int(),
    int(),
    |n: i64, _: &mut DynEnv| succeed::<_, Never, _>(n * 2),
  );
  group.server_stream(
    Procedure::server_stream("Lv.forever").public(),
    int(),
    int(),
    |n: i64, _: &mut DynEnv| succeed::<_, Never, _>(Stream::from_iterable(n..)),
  );
  group.server_stream(
    Procedure::server_stream("Lv.count").public(),
    int(),
    int(),
    |n: i64, _: &mut DynEnv| succeed::<_, Never, _>(Stream::from_iterable(1..=n)),
  );
  group.bidi(
    Procedure::bidi("Lv.echo").public(),
    int(),
    int(),
    |_: &mut DynEnv| succeed::<_, Never, _>(Duplex::new(|items: Stream<i64, Never, DynEnv>| items)),
  );
  group
}

struct TestNode {
  id: NodeId,
  rpc: MeshRpc,
}

fn node(name: &str, incarnation: u128) -> TestNode {
  let mut registry = BehaviorRegistry::new();
  MeshRpc::register_behavior(&mut registry);
  let id = NodeId::from_parts(name, incarnation);
  let mesh = Mesh::new(id.clone(), ProcessNode::new(NodeName::new(name)), registry);
  let rpc = MeshRpc::new(id.clone(), mesh).expect("mesh rpc");
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

/// `a` (the caller) and `b` (the server) joined through each other's ingress; the switch gates
/// b → a. Nothing ever calls `peer_down`.
struct Pair {
  a: TestNode,
  b: TestNode,
  ba: Arc<AtomicBool>,
}

fn pair() -> Pair {
  let (a, b) = (node("a@live", 1), node("b@live", 2));
  let ba = Arc::new(AtomicBool::new(true));
  a.rpc
    .mesh()
    .add_peer_node(&b.id, b.rpc.peer_sender(a.id.clone()));
  b.rpc
    .mesh()
    .add_peer_node(&a.id, gated(&ba, a.rpc.peer_sender(b.id.clone())));
  Pair { a, b, ba }
}

impl Pair {
  async fn serve(&self) -> id_effect_rpc::transport::mesh::MeshEndpoint {
    let endpoint = self.b.rpc.serve("Lv", group(), DynEnv::new());
    endpoint.start().expect("start");
    assert!(eventually(|| self.a.rpc.mesh().global().whereis("rpc:Lv").is_some()).await);
    endpoint
  }
}

async fn run<A: Send + 'static>(effect: Effect<A, MeshRpcError, ()>) -> Result<A, MeshRpcError> {
  match id_effect_axum::run_with_env((), move |_| effect).await {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(cause) => panic!("unexpected cause: {cause:?}"),
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

/// Read `stream` until it fails; the error.
async fn failure<O: serde::de::DeserializeOwned>(
  stream: &mut id_effect_rpc::client::mesh::MeshStream<O>,
) -> MeshRpcError {
  loop {
    match tokio::time::timeout(WAIT, stream.recv())
      .await
      .expect("in time")
      .expect("an item or the failure")
    {
      Ok(_) => {}
      Err(error) => return error,
    }
  }
}

// ---------------------------------------------------------------------------------------------
// F34: liveness
// ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mesh_stream_detects_silent_peer() {
  let p = pair();
  let clock = TestClock::new();
  let a = p.a.rpc.clone().with_liveness(
    MeshLiveness::new()
      .every(SECOND)
      .with_clock(clock.clone())
      .manual(),
  );
  assert_eq!(a.liveness().deadline(), SECOND * DEFAULT_SILENT_INTERVALS);
  let endpoint = p.serve().await;
  let lv = a.client("Lv");
  let mut forever = run(lv.server_stream::<_, i64, ()>("Lv.forever", &0_i64))
    .await
    .unwrap();
  assert_eq!(forever.recv().await.unwrap().unwrap(), 0);

  // A live peer answers every heartbeat: well past the deadline, the stream still runs.
  for _ in 0..(2 * DEFAULT_SILENT_INTERVALS) {
    clock.advance(SECOND);
    assert_eq!(a.heartbeat(), 0);
    tokio::time::sleep(Duration::from_millis(100)).await;
  }
  assert!(forever.recv().await.unwrap().is_ok());
  assert_eq!(endpoint.open_calls(), 1);

  // b → a goes silent; nothing reports b down. Past the deadline the stream fails, typed.
  p.ba.store(false, Ordering::SeqCst);
  let mut failed = 0;
  for _ in 0..=DEFAULT_SILENT_INTERVALS {
    clock.advance(SECOND);
    failed += a.heartbeat();
  }
  assert_eq!(failed, 1, "one stream, failed once");
  let error = failure(&mut forever).await;
  assert!(
    matches!(&error, MeshRpcError::PeerSilent { node, silent_for }
      if node == "b@live" && *silent_for > SECOND * DEFAULT_SILENT_INTERVALS),
    "{error:?}"
  );
  assert!(error.to_string().contains("b@live"));
  assert!(error.is_endpoint_gone());
  assert!(forever.recv().await.is_none());
  // The failed call is watched no more, and the endpoint was told to stop it.
  clock.advance(SECOND * 10);
  assert_eq!(a.heartbeat(), 0);
  assert!(eventually(|| endpoint.open_calls() == 0).await);

  // A finished stream is not watched either, and unary calls never are.
  p.ba.store(true, Ordering::SeqCst);
  let mut count = run(lv.server_stream::<_, i64, ()>("Lv.count", &2_i64))
    .await
    .unwrap();
  assert_eq!(count.recv().await.unwrap().unwrap(), 1);
  assert_eq!(count.recv().await.unwrap().unwrap(), 2);
  assert!(count.recv().await.is_none());
  clock.advance(SECOND * 10);
  assert_eq!(a.heartbeat(), 0);
  assert_eq!(
    run(lv.query::<_, i64, ()>("Lv.double", &4_i64))
      .await
      .unwrap(),
    8
  );
  drop(count);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mesh_liveness_runs_in_the_background() {
  let p = pair();
  let clock = TestClock::new();
  let a = p.a.rpc.clone().with_liveness(
    MeshLiveness::new()
      .every(Duration::from_millis(10))
      .silent_after(2)
      .with_clock(clock.clone()),
  );
  assert!(a.liveness().is_driven());
  p.serve().await;
  let lv = a.client("Lv");
  // Two-way streams are watched too.
  let (sink, mut echoed) = run(lv.bidi::<i64, i64, ()>("Lv.echo")).await.unwrap();
  sink.send(&7).await.unwrap();
  assert_eq!(echoed.recv().await.unwrap().unwrap(), 7);
  // The background rounds ping on their own; the test clock does not move, so nothing fails.
  tokio::time::sleep(Duration::from_millis(100)).await;
  sink.send(&8).await.unwrap();
  assert_eq!(echoed.recv().await.unwrap().unwrap(), 8);

  p.ba.store(false, Ordering::SeqCst);
  clock.advance(SECOND);
  let error = failure(&mut echoed).await;
  assert!(
    matches!(error, MeshRpcError::PeerSilent { .. }),
    "{error:?}"
  );
}

/// The status of a transport refusal, `0` for an I/O failure.
fn transport_failure<T>(result: Result<T, id_effect_rpc::service::TransportError>) -> Option<u16> {
  use id_effect_rpc::service::TransportError;
  match result {
    Err(TransportError::Refused { status, .. }) => Some(status),
    Err(TransportError::Io(_)) => Some(0),
    _ => None,
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mesh_stream_calls_without_an_endpoint_fail_typed() {
  use id_effect_rpc::contract::ProcedureKind;
  use id_effect_rpc::service::{RpcTransport as _, TransportCall};

  let p = pair();
  let nobody = p.a.rpc.client("Nobody");
  let gone = MeshRpcError::NoEndpoint("Nobody".to_owned());
  let feed = nobody.subscribe::<_, i64, ()>("Nobody.feed", &1_i64, None);
  assert_eq!(run(feed).await.unwrap_err(), gone);
  let sum = nobody.client_stream::<i64, i64, ()>("Nobody.sum");
  assert_eq!(run(sum).await.unwrap_err(), gone);
  let echo = nobody.bidi::<i64, i64, ()>("Nobody.echo");
  assert_eq!(run(echo).await.unwrap_err(), gone);

  // Through a generated client's transport: no endpoint is an I/O failure, a procedure the
  // schemas lack a 404, for every stream kind.
  let transport = MeshTransport::new(nobody, group());
  let call = |tag: &str, kind| TransportCall {
    tag: tag.to_owned(),
    version: 1,
    kind,
    payload: serde_json::json!(1),
  };
  let items = || tokio::sync::mpsc::channel(1).1;
  for (tag, expected) in [("Lv.count", Some(0)), ("Lv.nope", Some(404))] {
    let streamed = transport.client_stream(call(tag, ProcedureKind::ClientStream), items());
    assert_eq!(transport_failure(streamed.await), expected, "{tag}");
    let both = transport.bidi(call(tag, ProcedureKind::Bidi), items());
    assert_eq!(transport_failure(both.await), expected, "{tag}");
    let fed = transport.subscribe(call(tag, ProcedureKind::Subscription), None);
    assert_eq!(transport_failure(fed.await), expected, "{tag}");
  }
}

#[test]
fn mesh_liveness_is_finite_and_explicit() {
  let default = MeshLiveness::default();
  assert_eq!(default.interval(), DEFAULT_HEARTBEAT_INTERVAL);
  assert_eq!(
    default.deadline(),
    DEFAULT_HEARTBEAT_INTERVAL * DEFAULT_SILENT_INTERVALS
  );
  assert!(default.is_driven());
  let clamped = MeshLiveness::new()
    .every(Duration::MAX)
    .silent_after(u32::MAX)
    .manual();
  assert_eq!(clamped.interval(), Duration::from_secs(3600));
  assert_eq!(clamped.deadline(), Duration::from_secs(3600) * 100);
  assert!(!clamped.is_driven());
  assert_eq!(
    MeshLiveness::new().silent_after(0).deadline(),
    DEFAULT_HEARTBEAT_INTERVAL
  );
  assert!(format!("{clamped:?}").contains("MeshLiveness"));
}

// ---------------------------------------------------------------------------------------------
// F34: trace context through the generated client's transport
// ---------------------------------------------------------------------------------------------

/// Doubles numbers and counts.
#[id_effect_rpc::service(version = 1)]
trait Lv {
  /// `n × 2`.
  #[query]
  #[public]
  fn double(&self, n: i64) -> Effect<i64, Never, DynEnv>;

  /// 1, 2, …, n.
  #[server_stream]
  #[public]
  fn count(&self, n: i64) -> Effect<Stream<i64, Never, DynEnv>, Never, DynEnv>;
}

struct Lvs;

impl Lv for Lvs {
  fn double(&self, n: i64) -> Effect<i64, Never, DynEnv> {
    succeed(n * 2)
  }

  fn count(&self, n: i64) -> Effect<Stream<i64, Never, DynEnv>, Never, DynEnv> {
    succeed(Stream::from_iterable(1..=n))
  }
}

/// The `mesh.parent` of every receiving `rpc.request` span.
#[derive(Default)]
struct Parents(Mutex<Vec<String>>);

impl SpanHook for Parents {
  fn on_start(&self, span: &Span) {
    if span.name() == "rpc.request"
      && let Some((_, parent)) = span.attributes().iter().find(|(k, _)| k == "mesh.parent")
    {
      self.0.lock().unwrap().push(parent.clone());
    }
  }

  fn on_end(&self, _: &Span, _: Instant, _: ExitKind) {}
}

impl Parents {
  fn seen(&self, parent: &str) -> bool {
    self.0.lock().unwrap().iter().any(|p| p == parent)
  }

  fn len(&self) -> usize {
    self.0.lock().unwrap().len()
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mesh_transport_carries_trace_context() {
  let p = pair();
  let parents = Arc::new(Parents::default());
  let hook: Arc<dyn SpanHook> = parents.clone();
  let _ = p
    .b
    .rpc
    .clone()
    .with_trace_propagator(Arc::new(W3cTraceContext::new().with_receiver_hook(hook)));
  p.b
    .rpc
    .serve("Lv", LvService::group(Lvs), DynEnv::new())
    .start()
    .expect("start");
  assert!(eventually(|| p.a.rpc.mesh().global().whereis("rpc:Lv").is_some()).await);
  let transport = MeshTransport::new(p.a.rpc.client("Lv"), LvService::group(Lvs));
  assert!(transport.trace_context().is_empty());

  // A generated query inside a span: the receiving span's parent is the caller's.
  let client = LvService::client(transport.clone());
  let traced = Span::current()
    .flat_map(move |span| {
      let parent = traceparent_of(&span.expect("inside a span"));
      client.double(21).map(move |out| (parent, out))
    })
    .with_span("client.query");
  let (parent, out) = run_any(traced).await.unwrap();
  assert_eq!(out, 42);
  assert!(eventually(|| parents.seen(&parent)).await, "query");

  // A stream through the transport bound to the current span.
  let bound = transport.clone();
  let streamed = Span::current()
    .flat_map(move |span| {
      let parent = traceparent_of(&span.expect("inside a span"));
      bound
        .in_current_span()
        .flat_map(|t| LvService::client(t).count(3))
        .flat_map(|items| items.run_collect())
        .map(move |items| (parent, items))
    })
    .with_span("client.stream");
  let (parent, items) = run_any(streamed).await.unwrap();
  assert_eq!(items, [1, 2, 3]);
  assert!(eventually(|| parents.seen(&parent)).await, "stream");

  // An explicit context is carried as given; outside any span nothing is.
  let given = "00-0000000000000000000000000000abcd-00000000000000ef-01";
  let explicit = LvService::client(
    transport
      .clone()
      .with_trace_context(trace_pair("traceparent", given)),
  );
  assert_eq!(
    run_any(explicit.count(1).flat_map(|s| s.run_collect()))
      .await
      .unwrap(),
    [1]
  );
  assert!(eventually(|| parents.seen(given)).await, "explicit");
  let before = parents.len();
  let untraced = LvService::client(transport.clone());
  assert_eq!(run_any(untraced.double(1)).await.unwrap(), 2);
  let outside = transport.in_current_span::<Never, ()>();
  let unbound = match id_effect_axum::run_with_env((), move |_| outside).await {
    Exit::Success(t) => t,
    Exit::Failure(cause) => panic!("{cause:?}"),
  };
  assert!(unbound.trace_context().is_empty());
  tokio::time::sleep(Duration::from_millis(50)).await;
  assert_eq!(parents.len(), before);
}

// ---------------------------------------------------------------------------------------------
// F30: channels and mesh RPC over the node session pump
// ---------------------------------------------------------------------------------------------

#[cfg(feature = "channels")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn channels_over_node_session_pump() {
  use id_effect::Runtime;
  use id_effect_authz::{BasicPrincipal, CurrentPrincipal, Decision};
  use id_effect_node::{Cookie, MemoryNetwork};
  use id_effect_rpc::channels::{
    ChannelEvent, Channels, ChannelsConfig, JoinRequest, JoinSpec, ManualClock, connect_in_memory,
    join_policy_fn, register_hub,
  };

  const TOPIC: &str = "room:pump";
  // Each node runs channels (a hub) and mesh RPC on one mesh.
  let nodes: Vec<(NodeId, Mesh, MeshRpc)> = ["a@pump", "b@pump"]
    .iter()
    .enumerate()
    .map(|(i, name)| {
      let mut registry = BehaviorRegistry::new();
      register_hub(&mut registry);
      MeshRpc::register_behavior(&mut registry);
      let id = NodeId::from_parts(*name, i as u128 + 1);
      let mesh = Mesh::new(id.clone(), ProcessNode::new(NodeName::new(*name)), registry);
      let rpc = MeshRpc::new(id.clone(), mesh.clone()).expect("rpc");
      (id, mesh, rpc)
    })
    .collect();
  let clock = ManualClock::new();
  let channels: Vec<Channels<String>> = nodes
    .iter()
    .map(|(_, mesh, _)| {
      Channels::builder()
        .config(ChannelsConfig::new().with_heartbeat_interval(SECOND))
        .join_policy(join_policy_fn(|_: &JoinRequest| {
          Decision::allow_if(true, "open")
        }))
        .clock(clock.clone())
        .start_on_mesh(mesh)
        .expect("hub")
    })
    .collect();
  // The plain attach: sessions on the bare meshes, pumped by the node.
  let meshes: Vec<(NodeId, Mesh)> = nodes
    .iter()
    .map(|(id, m, _)| (id.clone(), m.clone()))
    .collect();
  let links = connect_in_memory(
    &MemoryNetwork::new(),
    &meshes,
    &Cookie::new("pump-channels-cookie-of-32-bytes!!"),
  )
  .await
  .expect("links");
  assert!(links.iter().all(|l| l.is_open()));
  assert!(
    MeshRpc::of(&nodes[0].1).is_some(),
    "the node's verified path"
  );

  // Channels: a subscriber on b hears a broadcast from a.
  let principal = CurrentPrincipal::new(BasicPrincipal::new("u"));
  let joined = channels[1]
    .join(JoinSpec::new(TOPIC, "conn-b"), principal)
    .provide_all(DynEnv::new());
  let mut sub = match Runtime::global().run(joined).await {
    Exit::Success(m) => m,
    Exit::Failure(cause) => panic!("join: {cause:?}"),
  };
  let deadline = Instant::now() + WAIT;
  while channels[0].nodes(TOPIC).is_empty() {
    assert!(Instant::now() < deadline, "did not converge");
    for c in &channels {
      c.heartbeat();
    }
    tokio::time::sleep(Duration::from_millis(10)).await;
  }
  let published = channels[0]
    .publish(TOPIC, None, &"over the pump".to_owned())
    .expect("publish");
  assert_eq!(published.nodes, 1);
  let body = loop {
    match tokio::time::timeout(WAIT, sub.recv())
      .await
      .expect("in time")
    {
      Ok(ChannelEvent::Message { body, .. }) => break body,
      Ok(_) => {}
      Err(e) => panic!("membership: {e}"),
    }
  };
  assert_eq!(body, "over the pump");

  // Mesh RPC over the same plain link: verified and stamped by the pump's ingress (the 0.12
  // F33 failure mode dropped these calls as unverified).
  nodes[1]
    .2
    .serve("Lv", group(), DynEnv::new())
    .start()
    .expect("start");
  assert!(eventually(|| nodes[0].1.global().whereis("rpc:Lv").is_some()).await);
  assert_eq!(
    run(
      nodes[0]
        .2
        .client("Lv")
        .query::<_, i64, ()>("Lv.double", &5_i64)
    )
    .await
    .unwrap(),
    10
  );
  for link in &links {
    let stats = link.stats();
    assert!(stats.delivered > 0, "{stats:?}");
    assert_eq!(stats.unverified, 0, "{stats:?}");
  }
  drop(links);
}
