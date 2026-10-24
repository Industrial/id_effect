//! UDP blocked: HTTP/3 on by default must never make the Relay unreachable (release spec risk 4).
//! The platform HTTP client learns `h3` from `Alt-Svc`, tries it, gives up after its fallback
//! timer and carries the call over TCP (and stops trying for a while); a WebTransport session
//! cannot start, and the same chat runs over the RPC WebSocket.
//!
//! Two ways to block UDP:
//!
//! - **CI** (`reference-relay` job): `RELAY_UDP_BLOCKED_PORT=<port>` with an iptables rule that
//!   drops UDP to that port; the node binds it, so the host's real HTTP/3 port is the blocked
//!   one.
//! - **Locally**: the node advertises (`Alt-Svc`, ahead of the host's own) a UDP port bound to a
//!   socket that never answers, and the WebTransport session dials that port: the same black
//!   hole.

mod common;

use std::time::{Duration, Instant};

use common::{Options, bearer_client, cluster, database, http_config, next, pump, run};
use id_effect_kit::platform::http::{Http3Client, Origin};
use relay::api::{JoinRoom, Outgoing};

/// The variable naming a UDP port the environment drops (the CI job's iptables rule).
const BLOCKED_PORT_ENV: &str = "RELAY_UDP_BLOCKED_PORT";

/// How long the client waits for an HTTP/3 handshake before it falls back to TCP.
const FALLBACK: Duration = Duration::from_millis(500);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relay_udp_blocked_falls_back_to_tcp() {
  let Some(db) = database("relay_udp_blocked_falls_back_to_tcp").await else {
    return;
  };
  let blocked: Option<u16> = std::env::var(BLOCKED_PORT_ENV)
    .ok()
    .map(|p| p.parse().expect("RELAY_UDP_BLOCKED_PORT is a port"));
  // Locally: a UDP port that never answers, advertised in place of the node's own.
  let blackhole = std::net::UdpSocket::bind((common::HOST, 0)).unwrap();
  let options = match blocked {
    Some(port) => Options {
      nodes: 1,
      port,
      ..Options::default()
    },
    None => Options {
      nodes: 1,
      advertise_h3_port: Some(blackhole.local_addr().unwrap().port()),
      ..Options::default()
    },
  };
  let c = cluster(db, options).await;
  let addr = c.nodes[0].addr();
  let udp = blocked.unwrap_or_else(|| blackhole.local_addr().unwrap().port());

  // The platform client with HTTP/3 on, as a mobile app would run it.
  let mut config = http_config(&c.certificate).http3(true);
  config.http3.fallback_timeout = FALLBACK;
  let client = Http3Client::from_config(&config);
  assert!(client.http3_available());
  let token = c.token("ada").await;
  let ada = bearer_client(&c.url(0), client.clone(), &token);
  // The first call goes over TCP and learns `h3` on the (blocked) port from `Alt-Svc`.
  run(ada.create_room("general".to_owned())).await.unwrap();
  let origin = Origin::new(common::HOST, addr.port());
  assert!(
    client
      .alt_svc()
      .lookup(&origin, std::time::Instant::now())
      .is_some(),
    "the node advertised HTTP/3"
  );
  // The next tries HTTP/3, gets no answer and completes over TCP within the fallback budget.
  let started = Instant::now();
  let ack = run(ada.send(Outgoing {
    room: "general".to_owned(),
    text: "over tcp".to_owned(),
  }))
  .await
  .unwrap();
  let took = started.elapsed();
  assert_eq!(ack.seq, 1);
  assert!(took >= FALLBACK, "HTTP/3 was tried first ({took:?})");
  assert!(took < FALLBACK * 10, "the fallback took {took:?}");
  // The origin is now known not to answer over UDP: no more waiting.
  assert!(
    client
      .alt_svc()
      .is_negative(&origin, std::time::Instant::now())
  );
  let started = Instant::now();
  run(ada.send(Outgoing {
    room: "general".to_owned(),
    text: "still tcp".to_owned(),
  }))
  .await
  .unwrap();
  assert!(started.elapsed() < FALLBACK, "{:?}", started.elapsed());

  // A WebTransport session cannot start over the blocked port ...
  let wt_addr = std::net::SocketAddr::new(addr.ip(), udp);
  let token = c.token("ada").await;
  let attempt = tokio::time::timeout(
    Duration::from_secs(3),
    common::Wt::connect(wt_addr, &c.certificate, Some(&token)),
  )
  .await;
  assert!(
    !matches!(attempt, Ok(Ok(_))),
    "WebTransport started over a blocked UDP port"
  );
  // ... so the client falls back to the RPC WebSocket on TCP, where the chat runs as before.
  let ws = c.ws(0, "ada").await;
  let joining = JoinRoom {
    room: "general".to_owned(),
    status: "online".to_owned(),
  };
  let (mut join, _j) = pump(run(ws.join(joining, None)).await.unwrap());
  assert_eq!(next(&mut join).await.value.kind, "joined");
  let (send, acks) = run(ws.chat()).await.unwrap();
  let (mut acks, _a) = pump(acks);
  run(send.send(Outgoing {
    room: "general".to_owned(),
    text: "over websocket".to_owned(),
  }))
  .await
  .unwrap();
  assert_eq!(next(&mut acks).await.seq, 3);
  loop {
    if let Some(message) = next(&mut join).await.value.message {
      assert_eq!(message.text, "over websocket");
      break;
    }
  }
  drop(send);
  c.shutdown().await;
}
