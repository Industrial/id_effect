//! Presence and the limits of channels (r12-e12-2-channels): convergence after a node goes down
//! (monitor-driven) or stops heartbeating (expiry under a manual clock), the caps, slow
//! subscribers, shutdown, and the join procedure on an `RpcGroup`.
#![cfg(feature = "channels")]

use std::time::Duration;

use id_effect::{DynEnv, Exit, Runtime};
use id_effect_authz::{BasicPrincipal, CurrentPrincipal, Decision};
use id_effect_node::{BehaviorRegistry, Cookie, MemoryNetwork, Mesh, NodeId};
use id_effect_process::{NodeName, ProcessNode};
use id_effect_rpc::channels::{
  AuthzJoin, ChannelError, ChannelEvent, Channels, ChannelsConfig, ConnectionId, JoinInput,
  JoinRequest, JoinSpec, ManualClock, Membership, Presence, SessionLink, connect_in_memory,
  join_policy_fn, register_hub,
};
use id_effect_rpc::handler::SubscriptionHandler;
use id_effect_rpc::{Procedure, RpcGroup};
use serde_json::json;

const TOPIC: &str = "room:presence";
const WAIT: Duration = Duration::from_secs(10);

fn mesh(name: &str) -> (NodeId, Mesh) {
  let mut registry = BehaviorRegistry::new();
  register_hub(&mut registry);
  let node = NodeName::new(name);
  let id = NodeId::fresh(&node);
  (id.clone(), Mesh::new(id, ProcessNode::new(node), registry))
}

fn open(mesh: &Mesh, config: ChannelsConfig, clock: &ManualClock) -> Channels<String> {
  Channels::builder()
    .config(config)
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

async fn join_as(
  channels: &Channels<String>,
  spec: JoinSpec,
  who: CurrentPrincipal,
) -> Result<Membership<String>, ChannelError> {
  let joined = channels.join(spec, who).provide_all(DynEnv::new());
  match Runtime::global().run(joined).await {
    Exit::Success(m) => Ok(m),
    Exit::Failure(id_effect::Cause::Fail(e)) => Err(e),
    Exit::Failure(cause) => panic!("join died: {cause:?}"),
  }
}

async fn join(
  channels: &Channels<String>,
  topic: &str,
  conn: &str,
) -> Result<Membership<String>, ChannelError> {
  join_as(channels, JoinSpec::new(topic, conn), member(conn)).await
}

async fn converge(nodes: &[Channels<String>], done: impl Fn() -> bool) {
  let deadline = tokio::time::Instant::now() + WAIT;
  while !done() {
    assert!(tokio::time::Instant::now() < deadline, "did not converge");
    for node in nodes {
      node.heartbeat();
    }
    tokio::time::sleep(Duration::from_millis(10)).await;
  }
}

async fn next(m: &mut Membership<String>) -> Result<ChannelEvent<String>, ChannelError> {
  tokio::time::timeout(WAIT, m.recv())
    .await
    .expect("event in time")
}

fn nodes_of(members: &[Presence]) -> Vec<&str> {
  members.iter().map(|p| p.node.as_str()).collect()
}

struct Cluster {
  nodes: Vec<Channels<String>>,
  subs: Vec<Membership<String>>,
  net: MemoryNetwork,
  clock: ManualClock,
  _links: Vec<SessionLink>,
}

/// Three nodes, one member on each (with metadata), every node seeing all three.
async fn cluster() -> Cluster {
  let meshes: Vec<(NodeId, Mesh)> = ["a@p", "b@p", "c@p"].iter().map(|n| mesh(n)).collect();
  let clock = ManualClock::new();
  let config = ChannelsConfig::new().with_heartbeat_interval(Duration::from_secs(1));
  let nodes: Vec<Channels<String>> = meshes
    .iter()
    .map(|(_, m)| open(m, config.clone(), &clock))
    .collect();
  let net = MemoryNetwork::new();
  let links = connect_in_memory(
    &net,
    &meshes,
    &Cookie::new("presence-test-cookie-32-bytes!!!!"),
  )
  .await
  .expect("mesh");
  let mut subs = Vec::new();
  for (i, node) in nodes.iter().enumerate() {
    let spec =
      JoinSpec::new(TOPIC, format!("conn{i}")).with_meta(json!({"status": "online", "i": i}));
    let mut m = join_as(node, spec, member(&format!("user{i}")))
      .await
      .expect("join");
    let Ok(ChannelEvent::Joined { me, .. }) = next(&mut m).await else {
      panic!("joined first")
    };
    assert_eq!(me.meta["i"], i);
    subs.push(m);
  }
  let view = nodes.clone();
  converge(&nodes, || view.iter().all(|n| n.members(TOPIC).len() == 3)).await;
  Cluster {
    nodes,
    subs,
    net,
    clock,
    _links: links,
  }
}

/// Wait for a presence diff on `m` whose leaves are on `node`.
async fn leave_of(m: &mut Membership<String>, node: &str) -> Presence {
  loop {
    if let Ok(ChannelEvent::Presence { diff, .. }) = next(m).await
      && let Some(gone) = diff.leaves.into_iter().find(|p| p.node == node)
    {
      return gone;
    }
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn presence_converges_after_node_down() {
  let mut c = cluster().await;
  // Remote members carry their metadata.
  let seen = c.nodes[0].members(TOPIC);
  let remote = seen.iter().find(|p| p.node == "c@p").expect("c's member");
  assert_eq!(remote.meta, json!({"status": "online", "i": 2}));
  assert_eq!(remote.key, "user2");
  // c loses the network: its sessions die, the mesh reports it down, the hub monitors fire.
  c.net.partition("a@p", "c@p");
  c.net.partition("b@p", "c@p");
  let gone = leave_of(&mut c.subs[0], "c@p").await;
  assert_eq!(gone.conn, "conn2");
  leave_of(&mut c.subs[1], "c@p").await;
  let view = c.nodes.clone();
  converge(&c.nodes[..2], || {
    nodes_of(&view[0].members(TOPIC)) == ["a@p", "b@p"]
      && nodes_of(&view[1].members(TOPIC)) == ["a@p", "b@p"]
      && view[0].nodes(TOPIC) == ["b@p".to_string()]
  })
  .await;
  // c, cut off, converges on itself alone.
  let view = c.nodes.clone();
  converge(&c.nodes[2..], || {
    nodes_of(&view[2].members(TOPIC)) == ["c@p"]
  })
  .await;
  // The other two still talk.
  let sent = c.nodes[1]
    .publish(TOPIC, Some("user1"), &"still here".to_string())
    .unwrap();
  assert_eq!(sent.nodes, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn presence_lapses_without_heartbeat_under_test_clock() {
  let mut c = cluster().await;
  // Time passes; a and b keep heartbeating, c (still connected) does not.
  c.clock.advance(Duration::from_secs(16));
  let view = c.nodes.clone();
  converge(&c.nodes[..2], || {
    nodes_of(&view[0].members(TOPIC)) == ["a@p", "b@p"]
  })
  .await;
  leave_of(&mut c.subs[0], "c@p").await;
  // c heartbeats again: its member comes back.
  converge(&c.nodes, || view[0].members(TOPIC).len() == 3).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn caps_bound_members_topics_and_payloads() {
  let (_, m) = mesh("caps@p");
  let config = ChannelsConfig::new()
    .with_max_members_per_topic(2)
    .with_max_topics_per_connection(2)
    .with_max_topics(3)
    .with_max_meta_bytes(16)
    .with_max_message_bytes(8);
  let ch = open(&m, config, &ManualClock::new());
  let _one = join(&ch, "t", "c1").await.unwrap();
  assert_eq!(
    join(&ch, "t", "c1").await.unwrap_err(),
    ChannelError::AlreadyJoined
  );
  let _two = join(&ch, "t", "c2").await.unwrap();
  assert_eq!(
    join(&ch, "t", "c3").await.unwrap_err(),
    ChannelError::TopicFull
  );
  // Topics per connection.
  let _u = join(&ch, "u", "c1").await.unwrap();
  assert_eq!(
    join(&ch, "v", "c1").await.unwrap_err(),
    ChannelError::TooManyTopics
  );
  // Topics per node.
  let _v = join(&ch, "v", "c4").await.unwrap();
  assert_eq!(
    join(&ch, "w", "c5").await.unwrap_err(),
    ChannelError::TooManyTopics
  );
  assert_eq!(ch.topics(), ["t", "u", "v"]);
  // Names, metadata and messages.
  assert_eq!(
    join(&ch, "", "c6").await.unwrap_err(),
    ChannelError::InvalidTopic
  );
  let big = JoinSpec::new("t", "c7").with_meta(json!({"pad": "x".repeat(32)}));
  assert_eq!(
    join_as(&ch, big, member("c7")).await.unwrap_err(),
    ChannelError::MetaTooLarge
  );
  assert_eq!(
    ch.publish("t", None, &"far too long".to_string())
      .unwrap_err(),
    ChannelError::MessageTooLarge
  );
  assert_eq!(
    ch.publish("a\u{0}b", None, &"x".into()).unwrap_err(),
    ChannelError::InvalidTopic
  );
  // A leave frees the slot.
  drop(_two);
  let _three = join(&ch, "t", "c3").await.unwrap();
  assert_eq!(ch.subscribers("t"), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slow_subscriber_is_dropped_as_lagged() {
  let (_, m) = mesh("slow@p");
  let ch = open(
    &m,
    ChannelsConfig::new().with_subscriber_buffer(3),
    &ManualClock::new(),
  );
  let mut slow = join(&ch, TOPIC, "slow").await.unwrap();
  let mut fast = join(&ch, TOPIC, "fast").await.unwrap();
  // slow holds Joined and fast's join diff; one message fills it, the next drops it. fast reads
  // as it goes.
  for n in 0..2 {
    while let Ok(Some(_)) = fast.try_recv() {}
    ch.publish(TOPIC, None, &format!("m{n}")).unwrap();
  }
  assert!(matches!(
    next(&mut slow).await,
    Ok(ChannelEvent::Joined { .. })
  ));
  assert!(matches!(
    next(&mut slow).await,
    Ok(ChannelEvent::Presence { .. })
  ));
  assert!(matches!(
    next(&mut slow).await,
    Ok(ChannelEvent::Message { .. })
  ));
  assert_eq!(next(&mut slow).await.unwrap_err(), ChannelError::Lagged);
  assert_eq!(slow.try_recv().unwrap_err(), ChannelError::Lagged);
  assert_eq!(ch.subscribers(TOPIC), 1, "only fast is left");
  assert_eq!(ch.members(TOPIC).len(), 1);
  // As a subscription stream: the items, then the Lagged failure.
  let stream = fast.into_subscription().into_stream();
  for n in 3..10 {
    ch.publish(TOPIC, None, &format!("m{n}")).unwrap();
  }
  let collected = stream.run_collect().provide_all(DynEnv::new());
  match Runtime::global().run(collected).await {
    Exit::Failure(id_effect::Cause::Fail(ChannelError::Lagged)) => {}
    other => panic!("expected Lagged, got {other:?}"),
  }
  assert_eq!(ch.subscribers(TOPIC), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_closes_subscribers_and_refuses_joins() {
  let (_, m) = mesh("down@p");
  let ch = open(&m, ChannelsConfig::new(), &ManualClock::new());
  let mut sub = join(&ch, TOPIC, "c1").await.unwrap();
  assert!(format!("{sub:?}").contains(TOPIC));
  assert_eq!(sub.topic(), TOPIC);
  ch.shutdown();
  assert!(matches!(
    next(&mut sub).await,
    Ok(ChannelEvent::Joined { .. })
  ));
  assert_eq!(next(&mut sub).await.unwrap_err(), ChannelError::Closed);
  assert_eq!(
    join(&ch, TOPIC, "c2").await.unwrap_err(),
    ChannelError::Closed
  );
  assert_eq!(
    ch.publish(TOPIC, None, &"x".into()).unwrap_err(),
    ChannelError::Closed
  );
  ch.shutdown();
  ch.heartbeat();
  // A second set of channels can take the namespace again once the first is gone.
  assert!(format!("{ch:?}").contains("down@p"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hub_setup_errors() {
  // Without register_hub the mesh cannot host a hub.
  let node = NodeName::new("bare@p");
  let bare = Mesh::new(
    NodeId::fresh(&node),
    ProcessNode::new(node),
    BehaviorRegistry::new(),
  );
  let err = Channels::<String>::builder()
    .start_on_mesh(&bare)
    .unwrap_err();
  assert!(matches!(err, ChannelError::Internal(_)));
  // One hub per namespace and node; namespaces are checked.
  let (_, m) = mesh("ns@p");
  let _first = Channels::<String>::builder().start_on_mesh(&m).unwrap();
  assert!(Channels::<String>::builder().start_on_mesh(&m).is_err());
  let other = Channels::<u64>::builder()
    .namespace("metrics")
    .start_on_mesh(&m)
    .unwrap();
  assert_eq!(other.node(), "ns@p");
  assert!(
    Channels::<u64>::builder()
      .namespace("a/b")
      .start_on_mesh(&m)
      .is_err()
  );
  assert!(
    Channels::<u64>::builder()
      .namespace("")
      .start_on_mesh(&m)
      .is_err()
  );
  assert!(format!("{:?}", Channels::<u64>::builder()).contains("default"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn join_procedure_runs_the_policy_with_the_call_principal() {
  let (_, m) = mesh("rpc@p");
  let ch: Channels<String> = Channels::builder()
    .join_policy(AuthzJoin::new(id_effect_authz::require_role("member")))
    .start_on_mesh(&m)
    .unwrap();
  let mut group = RpcGroup::new();
  ch.serve_joins(&mut group, Procedure::subscription("Chat.join").public());
  let contract = group.service_contract("Chat", 1);
  assert_eq!(contract.procedures[0].name, "join");

  let handler = ch.join_handler();
  let input = || JoinInput {
    topic: TOPIC.into(),
    meta: json!({"status": "online"}),
  };
  // Anonymous: 401.
  let mut env = DynEnv::new();
  let denied = handler.call(input(), None, &mut env).provide_all(env);
  assert!(matches!(
    Runtime::global().run(denied).await,
    Exit::Failure(id_effect::Cause::Fail(ChannelError::Unauthenticated))
  ));
  // A member on a known connection: the first item is the join.
  let mut env = DynEnv::new()
    .with(member("ada"))
    .with(ConnectionId("ws-1".into()));
  let joined = handler.call(input(), None, &mut env).provide_all(env);
  let Exit::Success(subscription) = Runtime::global().run(joined).await else {
    panic!("joined")
  };
  let first = subscription
    .into_stream()
    .take(1)
    .run_collect()
    .provide_all(DynEnv::new());
  let Exit::Success(items) = Runtime::global().run(first).await else {
    panic!("first item")
  };
  let ChannelEvent::Joined { me, .. } = &items[0].value else {
    panic!("joined first")
  };
  assert_eq!((me.key.as_str(), me.conn.as_str()), ("ada", "ws-1"));
  assert_eq!(items[0].cursor, None, "channels are live: no cursor");
  // The stream was dropped: ada left.
  assert!(ch.members(TOPIC).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn heartbeat_task_runs_until_dropped() {
  let (_, m) = mesh("tick@p");
  let ch: Channels<String> = Channels::builder()
    .config(ChannelsConfig::new().with_heartbeat_interval(Duration::from_millis(100)))
    .start_on_mesh(&m)
    .unwrap();
  let task = ch.spawn_heartbeat();
  tokio::time::sleep(Duration::from_millis(250)).await;
  assert!(format!("{task:?}").contains("HeartbeatTask"));
  drop(task);
  assert!(!ch.fresh_conn().is_empty());
  assert_eq!(ch.config().heartbeat_interval(), Duration::from_millis(100));
}
