//! Three Relay nodes on real QUIC and TLS on loopback (clients on the host's TCP and HTTP/3
//! port, the mesh on each node's `idm/1` endpoint, every session on a node session pump),
//! PostgreSQL in its own container: a room with members on every node,
//! messages over two-way streams, presence that converges when a node is lost, history that
//! resumes after a reconnect with no gap, and joins refused without membership.

mod common;

use std::time::Duration;

use common::{Options, Wt, WtCall, cluster, database, eventually, next, pump, run, run_typed};
use id_effect_kit::rpc::service::RpcCallError;
use id_effect_kit::rpc::wire::Cursor;
use relay::api::{ChatMessage, Invite, JoinRoom, Outgoing, RelayError, RoomEvent, topic};
use serde_json::{Value, json};

const ROOM: &str = "general";

fn invite(user: &str) -> Invite {
  Invite {
    room: ROOM.to_owned(),
    user: user.to_owned(),
  }
}

fn outgoing(text: &str) -> Outgoing {
  Outgoing {
    room: ROOM.to_owned(),
    text: text.to_owned(),
  }
}

fn joining() -> JoinRoom {
  JoinRoom {
    room: ROOM.to_owned(),
    status: "online".to_owned(),
  }
}

/// The typed failure of a call that must fail with one.
fn typed<A: std::fmt::Debug>(result: Result<A, RpcCallError<RelayError>>) -> RelayError {
  match result {
    Err(RpcCallError::Typed(e)) => e,
    other => panic!("expected a typed failure: {other:?}"),
  }
}

/// The next message event of a WebTransport join call (presence events skipped).
async fn wt_message(join: &mut WtCall) -> Value {
  loop {
    let event = join.item().await;
    if event["kind"] == "message" {
      return event["message"].clone();
    }
  }
}

/// The next message event of a pumped join stream (presence events skipped).
async fn ws_message(
  join: &mut tokio::sync::mpsc::UnboundedReceiver<
    Result<id_effect_kit::rpc::kinds::SubscriptionItem<RoomEvent>, String>,
  >,
) -> ChatMessage {
  loop {
    let event = next(join).await.value;
    if let Some(message) = event.message {
      return message;
    }
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relay_three_nodes_chat_over_http3() {
  let Some(db) = database("relay_three_nodes_chat_over_http3").await else {
    return;
  };
  let c = cluster(db, Options::default()).await;

  // The room and its members, over HTTP/3 only (the generated client on QUIC, no TCP).
  let ada = c.h3(0, "ada").await;
  run_typed(ada.create_room(ROOM.to_owned())).await.unwrap();
  for user in ["bob", "cy"] {
    run_typed(ada.invite(invite(user))).await.unwrap();
  }
  assert_eq!(
    typed(run_typed(ada.create_room(ROOM.to_owned())).await),
    RelayError::RoomExists {
      room: ROOM.to_owned()
    }
  );

  // Every user enters the room on a node of its own, over WebTransport (QUIC streams).
  let mut sessions = Vec::new();
  let mut joins = Vec::new();
  for (i, user) in ["ada", "bob", "cy"].into_iter().enumerate() {
    let wt = c.wt(i, user).await;
    let mut join = wt
      .call("Relay.join", json!({"room": ROOM, "status": "online"}))
      .await;
    let joined = join.item().await;
    assert_eq!(joined["kind"], "joined", "{joined}");
    sessions.push(wt);
    joins.push(join);
  }
  c.members_everywhere(ROOM, 3).await;

  // ada posts over a two-way stream on node a: acknowledged once stored, delivered on every
  // node.
  let mut chat = sessions[0].call("Relay.chat", Value::Null).await;
  chat
    .send(json!({"room": ROOM, "text": "hello over h3"}))
    .await;
  assert_eq!(chat.item().await, json!({"room": ROOM, "seq": 1}));
  for join in &mut joins {
    let message = wt_message(join).await;
    assert_eq!(message["text"], "hello over h3");
    assert_eq!(message["from"], "ada");
    assert_eq!(message["node"], "a@relay");
    assert_eq!(message["seq"], 1);
  }

  // bob posts with the unary procedure over HTTP/3 on node b.
  let bob = c.h3(1, "bob").await;
  let ack = run_typed(bob.send(outgoing("from b"))).await.unwrap();
  assert_eq!(ack.seq, 2);
  for join in &mut joins {
    let message = wt_message(join).await;
    assert_eq!(message["text"], "from b");
    assert_eq!(message["node"], "b@relay");
  }
  // A second message on the same stream keeps its order.
  chat.send(json!({"room": ROOM, "text": "again"})).await;
  assert_eq!(chat.item().await["seq"], 3);
  for join in &mut joins {
    assert_eq!(wt_message(join).await["text"], "again");
  }

  // Node c asks the others over mesh RPC: every node is up with one connection in the room.
  let view = run_typed(c.h3(2, "cy").await.cluster()).await.unwrap();
  let names: Vec<&str> = view.nodes.iter().map(|n| n.node.as_str()).collect();
  assert_eq!(names, ["c@relay", "a@relay", "b@relay"]);
  for node in &view.nodes {
    assert!(node.up, "{node:?}");
    assert_eq!(node.rooms.len(), 1, "{node:?}");
    assert_eq!(node.rooms[0].room, ROOM);
    assert_eq!(node.rooms[0].present, 1);
  }
  // The peers reached every node on its mesh endpoint (node i accepts the i nodes before it):
  // the clients' HTTP/3 and WebTransport above went to the host's endpoint instead.
  for (i, node) in c.nodes.iter().enumerate() {
    let stats = node.mesh_endpoint().stats();
    assert!(stats.accepted >= i as u64, "{:?}: {stats:?}", node.name());
  }
  // Every session runs on a node session pump that verified each envelope it delivered:
  // the channels and mesh RPC above crossed them, and nothing was dropped as unverified.
  for node in &c.nodes {
    let pumps = node.sessions();
    assert_eq!(pumps.len(), 2, "{:?}", node.name());
    for (peer, stats) in pumps {
      assert!(stats.delivered > 0, "{} <- {peer}: {stats:?}", node.name());
      assert_eq!(stats.unverified, 0, "{} <- {peer}: {stats:?}", node.name());
    }
  }
  for wt in &sessions {
    wt.close();
  }
  c.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relay_three_nodes_chat_over_websocket() {
  let Some(db) = database("relay_three_nodes_chat_over_websocket").await else {
    return;
  };
  let c = cluster(db, Options::default()).await;

  // The room over HTTP on TCP, then everything else on one WebSocket per user.
  let ada_http = c.http(0, "ada").await;
  run_typed(ada_http.create_room(ROOM.to_owned()))
    .await
    .unwrap();
  for user in ["bob", "cy"] {
    run_typed(ada_http.invite(invite(user))).await.unwrap();
  }
  let mut clients = Vec::new();
  let mut joins = Vec::new();
  for (i, user) in ["ada", "bob", "cy"].into_iter().enumerate() {
    let client = c.ws(i, user).await;
    let stream = run(client.join(joining(), None)).await.unwrap();
    let (mut rx, task) = pump(stream);
    let joined = next(&mut rx).await.value;
    assert_eq!(joined.kind, "joined");
    clients.push(client);
    joins.push((rx, task));
  }
  c.members_everywhere(ROOM, 3).await;

  // Two users post at once, each over its own two-way stream.
  let (ada_send, ada_acks) = run(clients[0].chat()).await.unwrap();
  let (cy_send, cy_acks) = run(clients[2].chat()).await.unwrap();
  let (mut ada_acks, _a) = pump(ada_acks);
  let (mut cy_acks, _c) = pump(cy_acks);
  for n in 0..3 {
    run(ada_send.send(outgoing(&format!("ada {n}"))))
      .await
      .unwrap();
    run(cy_send.send(outgoing(&format!("cy {n}"))))
      .await
      .unwrap();
  }
  let mut seqs = Vec::new();
  for _ in 0..3 {
    seqs.push(next(&mut ada_acks).await.seq);
    seqs.push(next(&mut cy_acks).await.seq);
  }
  seqs.sort_unstable();
  assert_eq!(seqs, [1, 2, 3, 4, 5, 6], "one number per message, no gap");

  // Every node delivers all six, each publisher's in its order.
  for (rx, _) in &mut joins {
    let mut seen = Vec::new();
    for _ in 0..6 {
      seen.push(ws_message(rx).await);
    }
    for from in ["ada", "cy"] {
      let texts: Vec<String> = seen
        .iter()
        .filter(|m| m.from == from)
        .map(|m| m.text.clone())
        .collect();
      assert_eq!(
        texts,
        (0..3).map(|n| format!("{from} {n}")).collect::<Vec<_>>()
      );
    }
  }

  // The history replays the room in its order, with a cursor per message.
  let history = run(clients[1].history(ROOM.to_owned(), None))
    .await
    .unwrap();
  let (mut history, _h) = pump(history);
  for seq in 1..=6 {
    let item = next(&mut history).await;
    assert_eq!(item.value.seq, seq);
    assert!(item.cursor.is_some());
  }
  drop((ada_send, cy_send));
  c.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relay_presence_converges_after_node_loss() {
  let Some(db) = database("relay_presence_converges_after_node_loss").await else {
    return;
  };
  let c = cluster(db, Options::default()).await;
  let ada = c.http(0, "ada").await;
  run_typed(ada.create_room(ROOM.to_owned())).await.unwrap();
  for user in ["bob", "cy"] {
    run_typed(ada.invite(invite(user))).await.unwrap();
  }
  let mut clients = Vec::new();
  let mut joins = Vec::new();
  for (i, user) in ["ada", "bob", "cy"].into_iter().enumerate() {
    let client = c.ws(i, user).await;
    let (mut rx, task) = pump(run(client.join(joining(), None)).await.unwrap());
    assert_eq!(next(&mut rx).await.value.kind, "joined");
    clients.push(client);
    joins.push((rx, task));
  }
  c.members_everywhere(ROOM, 3).await;

  // Node c goes away (its endpoint closes): a and b drop its member at once.
  c.nodes[2].shutdown().await;
  let t = topic(ROOM);
  eventually("a and b see two members", || {
    c.nodes[..2]
      .iter()
      .all(|n| n.state().channels().members(&t).len() == 2)
  })
  .await;
  for (rx, _) in joins.iter_mut().take(2) {
    loop {
      let event = next(rx).await.value;
      if event.kind == "presence" && !event.leaves.is_empty() {
        assert_eq!(event.leaves.len(), 1);
        assert_eq!(event.leaves[0].user, "cy");
        assert_eq!(event.leaves[0].node, "c@relay");
        break;
      }
    }
  }
  // The survivors still chat, and a newcomer on b sees exactly who is left.
  let ack = run_typed(ada.send(outgoing("still here"))).await.unwrap();
  assert_eq!(ack.seq, 1);
  for (rx, _) in joins.iter_mut().take(2) {
    assert_eq!(ws_message(rx).await.text, "still here");
  }
  let again = c.ws(1, "ada").await;
  let (mut rx, _t) = pump(run(again.join(joining(), None)).await.unwrap());
  let joined = next(&mut rx).await.value;
  let mut present: Vec<String> = joined.members.iter().map(|m| m.user.clone()).collect();
  present.sort();
  assert_eq!(present, ["ada", "ada", "bob"]);
  // a reports c down.
  let view = run_typed(ada.cluster()).await.unwrap();
  let lost = view.nodes.iter().find(|n| n.node == "c@relay").unwrap();
  assert!(!lost.up);
  c.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relay_resume_after_client_reconnect_no_gaps() {
  let Some(db) = database("relay_resume_after_client_reconnect_no_gaps").await else {
    return;
  };
  let c = cluster(db, Options::default()).await;
  let ada = c.http(0, "ada").await;
  run_typed(ada.create_room(ROOM.to_owned())).await.unwrap();
  run_typed(ada.invite(invite("bob"))).await.unwrap();
  for n in 1..=5 {
    run_typed(ada.send(outgoing(&format!("m{n}"))))
      .await
      .unwrap();
  }

  // bob follows the history on node b, reads three messages, and loses his connection.
  let bob = c.ws(1, "bob").await;
  let (mut history, task) = pump(run(bob.history(ROOM.to_owned(), None)).await.unwrap());
  let mut seen: Vec<i64> = Vec::new();
  let mut last: Option<Cursor> = None;
  for _ in 0..3 {
    let item = next(&mut history).await;
    seen.push(item.value.seq);
    last = item.cursor;
  }
  task.abort();
  drop(history);
  drop(bob);
  // The server releases the reader of the dropped connection.
  let b = c.nodes[1].state().clone();
  eventually("node b released the history reader", || {
    b.history().open_readers() == 0
  })
  .await;

  // Meanwhile the room goes on.
  for n in 6..=10 {
    run_typed(ada.send(outgoing(&format!("m{n}"))))
      .await
      .unwrap();
  }

  // bob comes back on another node with his last cursor: the rest, once each, in order, then
  // the live tail.
  let bob = c.ws(2, "bob").await;
  let (mut history, _t) = pump(run(bob.history(ROOM.to_owned(), last)).await.unwrap());
  for _ in 0..7 {
    let item = next(&mut history).await;
    assert_eq!(item.value.text, format!("m{}", item.value.seq));
    seen.push(item.value.seq);
  }
  run_typed(ada.send(outgoing("m11"))).await.unwrap();
  seen.push(next(&mut history).await.value.seq);
  assert_eq!(seen, (1..=11).collect::<Vec<i64>>(), "no gap, no repeat");
  // Nothing more arrives: no duplicate is in flight.
  assert!(
    tokio::time::timeout(Duration::from_millis(300), history.recv())
      .await
      .is_err()
  );
  // A cursor of another kind is refused with a typed error.
  let stale = Cursor::sequence(3);
  let refused = run_typed(
    bob
      .history(ROOM.to_owned(), Some(stale))
      .flat_map(|stream| stream.take(1).run_collect()),
  )
  .await;
  assert_eq!(typed(refused), RelayError::InvalidCursor);
  c.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relay_join_denied_without_membership() {
  let Some(db) = database("relay_join_denied_without_membership").await else {
    return;
  };
  let c = cluster(db, Options::default()).await;
  let ada = c.http(0, "ada").await;
  run_typed(ada.create_room("private".to_owned()))
    .await
    .unwrap();
  let not_a_member = RelayError::NotAMember {
    room: "private".to_owned(),
  };

  // mallory is signed in but not a member: every room procedure refuses, on every transport.
  let mallory = c.ws(1, "mallory").await;
  let joined = run_typed(
    mallory
      .join(
        JoinRoom {
          room: "private".to_owned(),
          status: "online".to_owned(),
        },
        None,
      )
      .flat_map(|stream| stream.take(1).run_collect()),
  )
  .await;
  assert_eq!(typed(joined), not_a_member);
  let history = run_typed(
    mallory
      .history("private".to_owned(), None)
      .flat_map(|stream| stream.take(1).run_collect()),
  )
  .await;
  assert_eq!(typed(history), not_a_member);
  let over_http = c.http(1, "mallory").await;
  let posted = run_typed(over_http.send(Outgoing {
    room: "private".to_owned(),
    text: "let me in".to_owned(),
  }))
  .await;
  assert_eq!(typed(posted), not_a_member);
  let invited = run_typed(over_http.invite(Invite {
    room: "private".to_owned(),
    user: "mallory".to_owned(),
  }))
  .await;
  assert_eq!(typed(invited), not_a_member);
  let session = c.wt(1, "mallory").await;
  let mut wt = session
    .call("Relay.join", json!({"room": "private", "status": "online"}))
    .await;
  let exit = wt.frame().await.expect("an exit");
  assert_eq!(exit["kind"], "exit", "{exit}");
  assert_eq!(exit["payload"]["error"]["_tag"], "NotAMember", "{exit}");
  assert!(c.nodes[1].state().channels().topics().is_empty());

  // Without a token nothing is served; the guard answers before any handler runs.
  let anonymous = Wt::connect(c.nodes[0].addr(), &c.certificate, None)
    .await
    .expect("the CONNECT itself is public");
  let mut call = anonymous
    .call("Relay.join", json!({"room": "private", "status": "online"}))
    .await;
  let exit = call.frame().await.expect("an exit");
  assert_eq!(exit["payload"]["error"]["_tag"], "Rejected", "{exit}");
  assert_eq!(exit["payload"]["error"]["status"], 401, "{exit}");
  let anonymous_http = common::http_client(
    &c.url(0),
    id_effect_kit::platform::http::ReqwestHttpClient::from_config(&common::http_config(
      &c.certificate,
    )),
  );
  assert!(
    run_typed(anonymous_http.create_room("mine".to_owned()))
      .await
      .is_err()
  );
  // A bad token is refused at the door (401 before any route).
  let forged = common::bearer_client(
    &c.url(0),
    id_effect_kit::platform::http::ReqwestHttpClient::from_config(&common::http_config(
      &c.certificate,
    )),
    "not.a.token",
  );
  assert!(run_typed(forged.cluster()).await.is_err());

  // Once invited, mallory gets in on the same node.
  run_typed(ada.invite(Invite {
    room: "private".to_owned(),
    user: "mallory".to_owned(),
  }))
  .await
  .unwrap();
  let joining_private = JoinRoom {
    room: "private".to_owned(),
    status: "online".to_owned(),
  };
  let (mut rx, _t) = pump(run(mallory.join(joining_private, None)).await.unwrap());
  assert_eq!(next(&mut rx).await.value.kind, "joined");
  assert!(
    id_effect::observability::posture::dangerous_opt_outs().is_empty(),
    "the Relay takes no dangerous opt-out"
  );
  c.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relay_send_retried_with_one_key_posts_once() {
  use id_effect_kit::platform::http::{HttpClient as _, HttpRequest, ReqwestHttpClient};

  let Some(db) = database("relay_send_retried_with_one_key_posts_once").await else {
    return;
  };
  let c = cluster(db, Options::default()).await;
  let ada = c.http(0, "ada").await;
  run_typed(ada.create_room(ROOM.to_owned())).await.unwrap();
  let token = c.token("ada").await;
  let http = ReqwestHttpClient::from_config(&common::http_config(&c.certificate));
  // The same mutation with the same key, first on node a, then retried on node b.
  let post = |node: usize, key: &str, text: &str| {
    let mut request = HttpRequest::post(
      format!("{}/rpc/Relay.send", c.url(node)),
      serde_json::to_vec(&json!({"room": ROOM, "text": text})).unwrap(),
    );
    for (name, value) in [
      ("authorization", format!("Bearer {token}")),
      ("content-type", "application/json".to_owned()),
      ("x-rpc", "1".to_owned()),
      ("idempotency-key", key.to_owned()),
    ] {
      request.headers.push((name.to_owned(), value));
    }
    http.execute(request)
  };
  let key = "relay-retry-key-0123456789";
  let first = run(post(0, key, "once")).await.unwrap();
  let retried = run(post(1, key, "once")).await.unwrap();
  assert_eq!(
    first.status,
    200,
    "{}",
    String::from_utf8_lossy(&first.body)
  );
  assert_eq!(retried.status, 200);
  assert_eq!(
    first.body, retried.body,
    "the stored result, on another node"
  );
  assert!(
    retried
      .headers
      .iter()
      .any(|(n, v)| n.eq_ignore_ascii_case("idempotency-replayed") && v == "true"),
    "{:?}",
    retried.headers
  );
  // Another payload under the same key is refused.
  let reused = run(post(2, key, "twice")).await.unwrap();
  assert_eq!(reused.status, 409);
  // A call that fails releases its key: the retry runs (and fails) again rather than wait.
  let failed = |node| {
    let mut request = HttpRequest::post(
      format!("{}/rpc/Relay.send", c.url(node)),
      serde_json::to_vec(&json!({"room": "nowhere", "text": "hi"})).unwrap(),
    );
    for (name, value) in [
      ("authorization", format!("Bearer {token}")),
      ("content-type", "application/json".to_owned()),
      ("x-rpc", "1".to_owned()),
      ("idempotency-key", "relay-failing-key-0123456789".to_owned()),
    ] {
      request.headers.push((name.to_owned(), value));
    }
    http.execute(request)
  };
  assert_eq!(run(failed(0)).await.unwrap().status, 403);
  assert_eq!(run(failed(1)).await.unwrap().status, 403);
  // The room holds the message once: the next one is number 2.
  let ack = run_typed(ada.send(outgoing("next"))).await.unwrap();
  assert_eq!(ack.seq, 2);
  c.shutdown().await;
}
