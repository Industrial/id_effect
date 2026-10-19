//! 090 — an effect-native WebSocket echo endpoint (feature `ws`).
//!
//! The route is public, accepts browser origins from the local dev servers only (by
//! configuration: `DEV_ORIGINS`), and keeps every secure default: 64 KiB frames, 100 messages per
//! second per socket, 16 sockets per client IP, a ping every 20 s and a 300 s idle timeout.
//!
//! ```text
//! cargo run -p id_effect_axum --features ws --example 090_ws_echo               # serve on :3000
//! cargo run -p id_effect_axum --features ws --example 090_ws_echo -- --self-test
//! ```

use std::net::SocketAddr;

use futures_util::{SinkExt, StreamExt};
use id_effect::{Effect, Never, succeed};
use id_effect_axum::RequestCaps;
use id_effect_axum::realtime::origin::DEV_ORIGINS;
use id_effect_axum::realtime::ws::{self, WsConfig, WsMessage, WsSocket};
use id_effect_axum::route::Router;
use id_effect_axum::{ConnectionLimits, serve_listener};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

/// Echoes every message, upper-casing text.
fn echo(_: &mut (), _caps: RequestCaps, socket: WsSocket) -> Effect<(), Never, ()> {
  let (incoming, out) = socket.into_parts::<Never, ()>();
  incoming.run_for_each_effect(move |msg| {
    let reply = match msg {
      WsMessage::Text(t) => WsMessage::Text(t.to_uppercase()),
      binary => binary,
    };
    // The client left: nothing more to do for this message.
    out.send(reply).catch(|_closed| succeed(()))
  })
}

fn app() -> axum::Router {
  let config = WsConfig::default()
    .allowed_origins(DEV_ORIGINS)
    .expect("valid origins");
  Router::new()
    .route(ws::endpoint("/echo", config, echo).public())
    .with_state(())
    .into_axum()
}

async fn self_test(addr: SocketAddr) {
  let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/echo"))
    .await
    .expect("upgrade");
  ws.send(Message::text("hello")).await.unwrap();
  assert_eq!(ws.next().await.unwrap().unwrap(), Message::text("HELLO"));
  ws.close(None).await.unwrap();

  let mut foreign = format!("ws://{addr}/echo").into_client_request().unwrap();
  foreign
    .headers_mut()
    .insert("origin", "https://evil.example".parse().unwrap());
  match tokio_tungstenite::connect_async(foreign).await {
    Err(tokio_tungstenite::tungstenite::Error::Http(res)) => assert_eq!(res.status(), 403),
    other => panic!(
      "a foreign origin must be refused, got {:?}",
      other.map(|_| ())
    ),
  }
  println!("090_ws_echo self-test: ok (echo round trip, foreign origin refused)");
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
  let test = std::env::args().any(|a| a == "--self-test");
  let bind = if test {
    "127.0.0.1:0"
  } else {
    "127.0.0.1:3000"
  };
  let listener = tokio::net::TcpListener::bind(bind).await?;
  let addr = listener.local_addr()?;
  let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
  let server = tokio::spawn(serve_listener(
    listener,
    app(),
    ConnectionLimits::new(),
    async move {
      let _ = stop_rx.await;
    },
  ));
  if test {
    self_test(addr).await;
    drop(stop_tx);
    return server.await.expect("server task");
  }
  println!("ws://{addr}/echo");
  let _keep = stop_tx;
  server.await.expect("server task")
}
