//! WebSocket handler smoke test.

use std::sync::Arc;

use axum::{Router, routing::get};
use futures_util::StreamExt;
use id_effect_dioxus::{RealtimeEvent, RealtimeHub, websocket_handler};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

#[tokio::test]
async fn websocket_handler_delivers_published_event() {
  let hub = Arc::new(RealtimeHub::new(8));
  let app = Router::new().route(
    "/ws",
    get({
      let hub = Arc::clone(&hub);
      move |ws| websocket_handler(ws, hub)
    }),
  );
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let addr = listener.local_addr().unwrap();
  tokio::spawn(async move {
    axum::serve(listener, app).await.unwrap();
  });
  let (mut socket, _) = connect_async(format!("ws://{addr}/ws"))
    .await
    .expect("connect");
  hub.publish(RealtimeEvent {
    topic: "t".into(),
    event: "ws-ping".into(),
    data_json: "{}".into(),
  });
  let msg = tokio::time::timeout(std::time::Duration::from_secs(2), socket.next())
    .await
    .expect("timeout")
    .expect("frame")
    .expect("ok");
  let text = match msg {
    Message::Text(t) => t.to_string(),
    other => panic!("unexpected frame: {other:?}"),
  };
  assert!(text.contains("ws-ping"));
  let _ = socket.close(None).await;
}
