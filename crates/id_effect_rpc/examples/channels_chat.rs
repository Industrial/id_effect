//! Example channels_chat — a chat room on three nodes: topics, presence with metadata, a
//! broadcast from any node to every node, a join refused by the policy, and a node lost.
//!
//! Run: `cargo run -p id_effect_rpc --features channels --example channels_chat`
//! (`-- --self-test` checks every step and exits non-zero on failure).

#[cfg(not(feature = "channels"))]
fn main() {
  eprintln!("channels_chat needs the `channels` feature: --features channels");
}

#[cfg(feature = "channels")]
#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
  let self_test = std::env::args().any(|a| a == "--self-test");
  match chat::run().await {
    Ok(()) if self_test => println!("self-test passed"),
    Ok(()) => {}
    Err(e) => {
      eprintln!("channels_chat: {e}");
      std::process::exit(1);
    }
  }
}

#[cfg(feature = "channels")]
mod chat {
  use std::time::Duration;

  use id_effect::{DynEnv, Exit, Runtime};
  use id_effect_authz::{BasicPrincipal, CurrentPrincipal, require_role};
  use id_effect_node::{BehaviorRegistry, Cookie, MemoryNetwork, Mesh, NodeId};
  use id_effect_process::{NodeName, ProcessNode};
  use id_effect_rpc::channels::{
    AuthzJoin, ChannelError, ChannelEvent, Channels, ChannelsConfig, ConnectionId, JoinInput,
    JoinSpec, Membership, connect_in_memory, register_hub,
  };
  use id_effect_rpc::handler::SubscriptionHandler;
  use id_effect_rpc::{Procedure, RpcGroup};
  use serde::{Deserialize, Serialize};
  use serde_json::json;

  /// One chat line.
  #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
  struct Line {
    text: String,
  }

  const ROOM: &str = "room:lobby";
  const WAIT: Duration = Duration::from_secs(10);

  fn check(ok: bool, what: &str) -> Result<(), String> {
    if ok {
      Ok(())
    } else {
      Err(format!("check failed: {what}"))
    }
  }

  fn node(name: &str) -> (NodeId, Mesh) {
    let mut registry = BehaviorRegistry::new();
    register_hub(&mut registry);
    let node = NodeName::new(name);
    let id = NodeId::fresh(&node);
    (id.clone(), Mesh::new(id, ProcessNode::new(node), registry))
  }

  fn chatter(id: &str) -> CurrentPrincipal {
    CurrentPrincipal::new(BasicPrincipal::new(id).with_role("chatter"))
  }

  /// Join the room from Rust (a WebSocket or SSE client goes through the join procedure, which
  /// ends in the same call).
  async fn join(
    channels: &Channels<Line>,
    who: CurrentPrincipal,
    conn: &str,
    status: &str,
  ) -> Result<Membership<Line>, ChannelError> {
    let spec = JoinSpec::new(ROOM, conn).with_meta(json!({"status": status}));
    let joined = channels.join(spec, who).provide_all(DynEnv::new());
    match Runtime::global().run(joined).await {
      Exit::Success(m) => Ok(m),
      Exit::Failure(id_effect::Cause::Fail(e)) => Err(e),
      Exit::Failure(cause) => Err(ChannelError::Internal(format!("{cause:?}"))),
    }
  }

  async fn until(what: &str, done: impl Fn() -> bool) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + WAIT;
    while !done() {
      if tokio::time::Instant::now() > deadline {
        return Err(format!("timed out waiting for {what}"));
      }
      tokio::time::sleep(Duration::from_millis(20)).await;
    }
    Ok(())
  }

  async fn next_line(m: &mut Membership<Line>) -> Result<(String, Line), String> {
    loop {
      let event = tokio::time::timeout(WAIT, m.recv())
        .await
        .map_err(|_| "no message in time".to_string())?
        .map_err(|e| e.to_string())?;
      if let ChannelEvent::Message { from, body, .. } = event {
        return Ok((from.unwrap_or_default(), body));
      }
    }
  }

  pub async fn run() -> Result<(), String> {
    let names = ["a@chat", "b@chat", "c@chat"];
    let meshes: Vec<(NodeId, Mesh)> = names.iter().map(|n| node(n)).collect();
    let config = ChannelsConfig::new().with_heartbeat_interval(Duration::from_millis(200));
    let mut nodes = Vec::new();
    for (_, mesh) in &meshes {
      let channels = Channels::<Line>::builder()
        .config(config.clone())
        // Joining needs the role "chatter"; everyone else is refused (the default refuses all).
        .join_policy(AuthzJoin::new(require_role("chatter")))
        .start_on_mesh(mesh)
        .map_err(|e| e.to_string())?;
      nodes.push(channels);
    }
    let _heartbeats: Vec<_> = nodes.iter().map(Channels::spawn_heartbeat).collect();
    let net = MemoryNetwork::new();
    let _links = connect_in_memory(
      &net,
      &meshes,
      &Cookie::new("channels-chat-cookie-32-bytes!!!"),
    )
    .await
    .map_err(|e| e.to_string())?;

    // The join procedure as a service would serve it (WebSocket / SSE transports).
    let mut group = RpcGroup::new();
    nodes[0].serve_joins(&mut group, Procedure::subscription("Chat.join").public());
    let contract = group.service_contract("Chat", 1);
    println!(
      "procedure: Chat.{} ({:?})",
      contract.procedures[0].name, contract.procedures[0].kind
    );

    // Three people on three nodes.
    let mut alice = join(&nodes[0], chatter("alice"), "ws-alice", "online")
      .await
      .map_err(|e| e.to_string())?;
    let mut bob = join(&nodes[1], chatter("bob"), "ws-bob", "online")
      .await
      .map_err(|e| e.to_string())?;
    let mut carol = join(&nodes[2], chatter("carol"), "sse-carol", "away")
      .await
      .map_err(|e| e.to_string())?;
    let view = nodes.clone();
    until("presence on every node", || {
      view.iter().all(|n| n.members(ROOM).len() == 3)
    })
    .await?;
    for member in nodes[0].members(ROOM) {
      println!(
        "a@chat sees {} on {} ({})",
        member.key, member.node, member.meta["status"]
      );
    }

    // Anyone may speak; everyone hears it, on every node, in order per speaker.
    nodes[1]
      .publish(
        ROOM,
        Some("bob"),
        &Line {
          text: "hi all".into(),
        },
      )
      .map_err(|e| e.to_string())?;
    nodes[2]
      .publish(
        ROOM,
        Some("carol"),
        &Line {
          text: "hello from c".into(),
        },
      )
      .map_err(|e| e.to_string())?;
    for (who, sub) in [
      ("alice", &mut alice),
      ("bob", &mut bob),
      ("carol", &mut carol),
    ] {
      let mut heard = vec![next_line(sub).await?, next_line(sub).await?];
      heard.sort_by(|x, y| x.0.cmp(&y.0));
      println!("{who} heard {heard:?}");
      check(
        heard[0]
          == (
            "bob".into(),
            Line {
              text: "hi all".into(),
            },
          ),
        "bob's line",
      )?;
      check(heard[1].0 == "carol", "carol's line")?;
    }

    // Mallory has no role: the join policy refuses; the procedure answers 403.
    let mut env = DynEnv::new()
      .with(CurrentPrincipal::new(BasicPrincipal::new("mallory")))
      .with(ConnectionId("ws-mallory".into()));
    let input = JoinInput {
      topic: ROOM.into(),
      meta: json!(null),
    };
    let refused = nodes[0]
      .join_handler()
      .call(input, None, &mut env)
      .provide_all(env);
    let refused = matches!(
      Runtime::global().run(refused).await,
      Exit::Failure(id_effect::Cause::Fail(ChannelError::JoinDenied))
    );
    println!("mallory refused: {refused}");
    check(refused, "mallory is refused")?;

    // Node c drops off the network: a and b see carol leave.
    net.partition("a@chat", "c@chat");
    net.partition("b@chat", "c@chat");
    until("carol's leave on a and b", || {
      view[..2]
        .iter()
        .all(|n| n.members(ROOM).iter().all(|m| m.node != "c@chat"))
    })
    .await?;
    println!(
      "after c@chat went down, a@chat sees {:?}",
      nodes[0]
        .members(ROOM)
        .iter()
        .map(|m| &m.key)
        .collect::<Vec<_>>()
    );
    check(nodes[0].members(ROOM).len() == 2, "two left")?;

    // Leaving is dropping the membership.
    drop(bob);
    until("bob's leave", || nodes[0].members(ROOM).len() == 1).await?;
    check(
      id_effect::observability::posture::dangerous_opt_outs().is_empty(),
      "no opt-outs",
    )?;
    drop((alice, carol));
    Ok(())
  }
}
