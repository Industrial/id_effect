//! Channels across three real nodes in one process (r12-e12-2-channels): meshes joined by
//! authenticated sessions — over the in-memory transport under a manual clock, and over QUIC
//! with mutual TLS — with one hub process per node.
#![cfg(feature = "channels")]

use std::time::Duration;

use id_effect::{DynEnv, Exit, Runtime};
use id_effect_authz::{BasicPrincipal, CurrentPrincipal, Decision};
use id_effect_node::transport::certs::NodeCertificate;
use id_effect_node::transport::quic::{QuicConfig, QuicTransport};
use id_effect_node::{BehaviorRegistry, Cookie, MemoryNetwork, Mesh, NodeId, Session, Transport};
use id_effect_process::{NodeName, ProcessNode};
use id_effect_rpc::channels::{
  ChannelError, ChannelEvent, Channels, ChannelsConfig, JoinRequest, JoinSpec, ManualClock,
  Membership, SessionLink, attach_session, connect_in_memory, join_policy_fn, register_hub,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Chat {
  n: u32,
  text: String,
}

const TOPIC: &str = "room:general";
const WAIT: Duration = Duration::from_secs(10);

fn cookie() -> Cookie {
  Cookie::new("channels-test-cookie-of-32-bytes!!")
}

fn mesh(name: &str) -> (NodeId, Mesh) {
  let mut registry = BehaviorRegistry::new();
  register_hub(&mut registry);
  let node = NodeName::new(name);
  let id = NodeId::fresh(&node);
  (id.clone(), Mesh::new(id, ProcessNode::new(node), registry))
}

/// Members of the role "member" may join.
fn channels(mesh: &Mesh, clock: &ManualClock) -> Channels<Chat> {
  Channels::builder()
    .config(ChannelsConfig::new().with_heartbeat_interval(Duration::from_secs(1)))
    .join_policy(join_policy_fn(|join: &JoinRequest| {
      Decision::allow_if(join.principal.has_role("member"), "members only")
    }))
    .clock(clock.clone())
    .start_on_mesh(mesh)
    .expect("hub")
}

fn member(id: &str) -> CurrentPrincipal {
  CurrentPrincipal::new(BasicPrincipal::new(id).with_role("member"))
}

async fn join(
  channels: &Channels<Chat>,
  conn: &str,
  who: &str,
) -> Result<Membership<Chat>, ChannelError> {
  let spec = JoinSpec::new(TOPIC, conn).with_meta(serde_json::json!({"who": who}));
  let joined = channels.join(spec, member(who)).provide_all(DynEnv::new());
  match Runtime::global().run(joined).await {
    Exit::Success(m) => Ok(m),
    Exit::Failure(id_effect::Cause::Fail(e)) => Err(e),
    Exit::Failure(cause) => panic!("join died: {cause:?}"),
  }
}

/// Heartbeat every node until `done` holds (bounded by [`WAIT`]).
async fn converge(nodes: &[Channels<Chat>], done: impl Fn() -> bool) {
  let deadline = tokio::time::Instant::now() + WAIT;
  while !done() {
    assert!(tokio::time::Instant::now() < deadline, "did not converge");
    for node in nodes {
      node.heartbeat();
    }
    tokio::time::sleep(Duration::from_millis(10)).await;
  }
}

async fn next(m: &mut Membership<Chat>) -> ChannelEvent<Chat> {
  tokio::time::timeout(WAIT, m.recv())
    .await
    .expect("event in time")
    .expect("membership open")
}

/// The next message, skipping presence events.
async fn next_message(m: &mut Membership<Chat>) -> (String, Chat) {
  loop {
    if let ChannelEvent::Message { node, body, .. } = next(m).await {
      return (node, body);
    }
  }
}

struct Cluster {
  nodes: Vec<Channels<Chat>>,
  _links: Vec<SessionLink>,
  clock: ManualClock,
}

async fn memory_cluster() -> Cluster {
  let names = ["a@test", "b@test", "c@test"];
  let meshes: Vec<(NodeId, Mesh)> = names.iter().map(|n| mesh(n)).collect();
  let clock = ManualClock::new();
  let nodes: Vec<Channels<Chat>> = meshes.iter().map(|(_, m)| channels(m, &clock)).collect();
  let net = MemoryNetwork::new();
  let links = connect_in_memory(&net, &meshes, &cookie())
    .await
    .expect("mesh");
  Cluster {
    nodes,
    _links: links,
    clock,
  }
}

/// One subscriber per node, every node seeing all three members.
async fn joined_everywhere(cluster: &Cluster) -> Vec<Membership<Chat>> {
  let mut subs = Vec::new();
  for (i, node) in cluster.nodes.iter().enumerate() {
    let who = format!("user{i}");
    let mut m = join(node, &format!("conn{i}"), &who).await.expect("join");
    assert!(matches!(next(&mut m).await, ChannelEvent::Joined { .. }));
    subs.push(m);
  }
  let nodes = cluster.nodes.clone();
  converge(&cluster.nodes, || {
    nodes
      .iter()
      .all(|n| n.members(TOPIC).len() == 3 && n.nodes(TOPIC).len() == 2)
  })
  .await;
  subs
}

async fn drain(m: &mut Membership<Chat>) {
  while let Ok(Some(_)) = m.try_recv() {}
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn three_node_broadcast_under_test_clock() {
  let cluster = memory_cluster().await;
  let mut subs = joined_everywhere(&cluster).await;
  for m in &mut subs {
    drain(m).await;
  }
  // A broadcast on any node reaches the subscribers on every node.
  for (i, node) in cluster.nodes.iter().enumerate() {
    let msg = Chat {
      n: u32::try_from(i).unwrap(),
      text: format!("hello from {}", node.node()),
    };
    let published = node.publish(TOPIC, Some("user"), &msg).expect("publish");
    assert_eq!(published.local, 1);
    assert_eq!(published.nodes, 2, "one cast per other node");
    for m in &mut subs {
      let (from, body) = next_message(m).await;
      assert_eq!(from, node.node());
      assert_eq!(body, msg);
    }
  }
  // Manual time: nothing lapsed while the nodes heartbeat.
  cluster.clock.advance(Duration::from_secs(1));
  for node in &cluster.nodes {
    node.heartbeat();
    assert_eq!(node.members(TOPIC).len(), 3);
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn publisher_order_preserved_per_topic() {
  let cluster = memory_cluster().await;
  let mut subs = joined_everywhere(&cluster).await;
  for m in &mut subs {
    drain(m).await;
  }
  // Two publishers at once, interleaved; each one's messages arrive in its own order.
  let (a, b) = (cluster.nodes[0].clone(), cluster.nodes[1].clone());
  let send = |node: Channels<Chat>| {
    tokio::task::spawn_blocking(move || {
      for n in 0..100 {
        let msg = Chat {
          n,
          text: node.node().to_string(),
        };
        node.publish(TOPIC, None, &msg).expect("publish");
      }
    })
  };
  let (ra, rb) = tokio::join!(send(a), send(b));
  ra.unwrap();
  rb.unwrap();
  for m in &mut subs {
    let mut last: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    let mut got = 0;
    while got < 200 {
      let (node, body) = next_message(m).await;
      if let Some(prev) = last.insert(node.clone(), body.n) {
        assert_eq!(body.n, prev + 1, "in publish order from {node}");
      } else {
        assert_eq!(body.n, 0);
      }
      got += 1;
    }
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn leave_stops_delivery() {
  let cluster = memory_cluster().await;
  let mut subs = joined_everywhere(&cluster).await;
  let c = subs.pop().unwrap();
  let mut a = subs.remove(0);
  drain(&mut a).await;
  drop(c);
  // The leave reaches the other nodes as a presence diff, and c drops out of the group.
  loop {
    if let ChannelEvent::Presence { diff, .. } = next(&mut a).await {
      assert_eq!(diff.leaves.len(), 1);
      assert_eq!(diff.leaves[0].node, "c@test");
      break;
    }
  }
  let nodes = cluster.nodes.clone();
  converge(&cluster.nodes, || {
    nodes[0].nodes(TOPIC) == vec!["b@test".to_string()] && nodes[2].subscribers(TOPIC) == 0
  })
  .await;
  let published = cluster.nodes[0]
    .publish(
      TOPIC,
      None,
      &Chat {
        n: 1,
        text: "after".into(),
      },
    )
    .unwrap();
  assert_eq!(published.nodes, 1, "only b is still interested");
  assert!(cluster.nodes[2].topics().is_empty());
  assert!(cluster.nodes[2].members(TOPIC).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn channel_join_denied_by_default_policy() {
  let (_, m) = mesh("solo@test");
  let closed = Channels::<Chat>::builder().start_on_mesh(&m).expect("hub");
  // Default policy: an authenticated principal is denied (403), an anonymous one 401.
  assert_eq!(
    join(&closed, "c1", "alice").await.unwrap_err(),
    ChannelError::JoinDenied
  );
  let anonymous = closed
    .join(JoinSpec::new(TOPIC, "c2"), CurrentPrincipal::anonymous())
    .provide_all(DynEnv::new());
  assert!(matches!(
    Runtime::global().run(anonymous).await,
    Exit::Failure(id_effect::Cause::Fail(ChannelError::Unauthenticated))
  ));
  assert!(closed.topics().is_empty(), "nothing was joined");
  assert!(id_effect::observability::posture::dangerous_opt_outs().is_empty());
}

// ---- QUIC with mutual TLS ------------------------------------------------------------------

/// Three nodes with self-signed certificates for "cluster", each pinning all three in both
/// directions: a server certificate and a client certificate are both required and checked.
struct Pinned {
  certs: Vec<NodeCertificate>,
}

impl Pinned {
  fn new(n: usize) -> Self {
    Self {
      certs: (0..n)
        .map(|_| NodeCertificate::self_signed("cluster").expect("cert"))
        .collect(),
    }
  }

  fn config(&self, i: usize, trusted: &[usize]) -> QuicConfig {
    let mut config = QuicConfig::from_certificate(&self.certs[i]).expect("config");
    for &t in trusted {
      config = config.with_pinned_peer_cert_sha256(self.certs[t].cert_sha256());
    }
    config
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn three_node_broadcast_over_quic_mtls() {
  let pki = Pinned::new(4);
  let all = [0, 1, 2];
  let meshes: Vec<(NodeId, Mesh)> = ["a@quic", "b@quic", "c@quic"]
    .iter()
    .map(|n| mesh(n))
    .collect();
  let clock = ManualClock::new();
  let nodes: Vec<Channels<Chat>> = meshes.iter().map(|(_, m)| channels(m, &clock)).collect();
  let mut listeners = Vec::new();
  for i in 0..3 {
    let listener = QuicTransport::new(pki.config(i, &all))
      .listen("127.0.0.1:0")
      .await
      .expect("listen");
    listeners.push(listener);
  }
  let mut links = Vec::new();
  let cookie = cookie();
  for i in 0..3 {
    for j in i + 1..3 {
      let addr = listeners[j].local_addr();
      let dialer = QuicTransport::try_new(pki.config(i, &all)).expect("trust set");
      // The dialer opens its stream with the first handshake frame: accept and respond while
      // the dialer connects and initiates.
      let (a, b) = tokio::join!(
        async {
          let dialed = dialer.connect(&addr).await.expect("dial");
          Session::initiate(dialed, &meshes[i].0, &cookie).await
        },
        async {
          let accepted = listeners[j].accept().await.expect("accept");
          Session::respond(accepted, &meshes[j].0, &cookie).await
        },
      );
      links.push(attach_session(&meshes[i].1, a.expect("session")));
      links.push(attach_session(&meshes[j].1, b.expect("session")));
    }
  }
  // A fourth node whose certificate nobody pins is refused before any session exists.
  let rogue = QuicTransport::try_new(pki.config(3, &all)).expect("trust set");
  let addr = listeners[0].local_addr();
  let rogue_id = NodeId::from_parts("rogue@quic", 1);
  let (dialed, accepted) = tokio::join!(
    tokio::time::timeout(Duration::from_secs(5), async {
      let conn = rogue.connect(&addr).await.map_err(|e| e.to_string())?;
      Session::initiate(conn, &rogue_id, &cookie)
        .await
        .map_err(|e| e.to_string())
    }),
    tokio::time::timeout(Duration::from_secs(5), async {
      let conn = listeners[0].accept().await.map_err(|e| e.to_string())?;
      Session::respond(conn, &meshes[0].0, &cookie)
        .await
        .map_err(|e| e.to_string())
    }),
  );
  assert!(
    !matches!(accepted, Ok(Ok(_))),
    "the listener opened a session to an unpinned peer"
  );
  assert!(
    !matches!(dialed, Ok(Ok(_))),
    "an unpinned peer completed a session"
  );

  let mut subs = Vec::new();
  for (i, node) in nodes.iter().enumerate() {
    let mut m = join(node, &format!("conn{i}"), &format!("user{i}"))
      .await
      .expect("join");
    assert!(matches!(next(&mut m).await, ChannelEvent::Joined { .. }));
    subs.push(m);
  }
  let view = nodes.clone();
  converge(&nodes, || {
    view
      .iter()
      .all(|n| n.members(TOPIC).len() == 3 && n.nodes(TOPIC).len() == 2)
  })
  .await;
  let msg = Chat {
    n: 7,
    text: "over QUIC".into(),
  };
  assert_eq!(nodes[1].publish(TOPIC, None, &msg).unwrap().nodes, 2);
  for m in &mut subs {
    let (from, body) = next_message(m).await;
    assert_eq!(from, "b@quic");
    assert_eq!(body, msg);
  }
  assert!(id_effect::observability::posture::dangerous_opt_outs().is_empty());
  drop(links);
}
