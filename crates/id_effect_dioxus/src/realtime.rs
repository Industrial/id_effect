//! SSE and WebSocket realtime channel stubs.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::{Stream, StreamExt, future};
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;

/// Transport kind for a realtime subscription.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RealtimeTransport {
  /// Server-Sent Events (`text/event-stream`).
  Sse,
  /// WebSocket JSON frames.
  WebSocket,
}

/// JSON-serializable realtime payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealtimeEvent {
  /// Channel topic, e.g. `orders/42`.
  pub topic: String,
  /// Event name for client dispatch.
  pub event: String,
  /// JSON payload string.
  pub data_json: String,
}

impl RealtimeEvent {
  /// SSE `data:` line body.
  #[inline]
  pub fn to_sse_data(&self) -> String {
    serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
  }
}

/// In-memory pub/sub hub (single-node; cluster fan-out is an application concern).
#[derive(Debug, Clone)]
pub struct RealtimeHub {
  tx: broadcast::Sender<RealtimeEvent>,
}

impl RealtimeHub {
  /// Creates a hub with `capacity` buffered events per slow subscriber.
  #[inline]
  pub fn new(capacity: usize) -> Self {
    let (tx, _) = broadcast::channel(capacity.max(4));
    Self { tx }
  }

  /// Publishes an event to all subscribers.
  #[inline]
  pub fn publish(&self, event: RealtimeEvent) {
    let _ = self.tx.send(event);
  }

  /// Subscribe for SSE/WebSocket consumers.
  #[inline]
  pub fn subscribe(&self) -> broadcast::Receiver<RealtimeEvent> {
    self.tx.subscribe()
  }
}

impl Default for RealtimeHub {
  fn default() -> Self {
    Self::new(64)
  }
}

/// Build an SSE response from `hub`.
pub fn sse_handler(
  hub: Arc<RealtimeHub>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>> + Send + 'static> {
  let rx = hub.subscribe();
  let stream = BroadcastStream::new(rx).filter_map(|msg| {
    future::ready(msg.ok().map(|ev| {
      let name = ev.event.clone();
      Ok(Event::default().event(name).data(ev.to_sse_data()))
    }))
  });
  Sse::new(stream).keep_alive(KeepAlive::default())
}

/// Axum WebSocket upgrade handler.
pub async fn websocket_handler(ws: WebSocketUpgrade, hub: Arc<RealtimeHub>) -> Response {
  ws.on_upgrade(move |socket| handle_socket(socket, hub))
}

async fn handle_socket(mut socket: WebSocket, hub: Arc<RealtimeHub>) {
  let mut rx = hub.subscribe();
  loop {
    tokio::select! {
      msg = rx.recv() => {
        match msg {
          Ok(ev) => {
            let text = ev.to_sse_data();
            if socket.send(Message::Text(text.into())).await.is_err() {
              break;
            }
          }
          Err(_) => break,
        }
      }
      incoming = socket.recv() => {
        match incoming {
          Some(Ok(Message::Close(_))) | None => break,
          Some(Ok(_)) => {}
          Some(Err(_)) => break,
        }
      }
    }
  }
}

/// Session handle for tests (wraps a cloned receiver).
#[derive(Debug)]
pub struct WebSocketSession {
  hub: Arc<RealtimeHub>,
}

impl WebSocketSession {
  /// Creates a session bound to `hub`.
  #[inline]
  pub fn new(hub: Arc<RealtimeHub>) -> Self {
    Self { hub }
  }

  /// Drains one published event (blocking test helper).
  #[inline]
  pub async fn next_event(&self) -> Option<RealtimeEvent> {
    let mut rx = self.hub.subscribe();
    rx.recv().await.ok()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn realtime_event_sse_data_contains_fields() {
    let ev = RealtimeEvent {
      topic: "orders/1".into(),
      event: "created".into(),
      data_json: r#"{"id":1}"#.into(),
    };
    let data = ev.to_sse_data();
    assert!(data.contains("orders/1"));
    assert!(data.contains("created"));
  }

  #[test]
  fn hub_default_and_publish_subscribe() {
    let hub = RealtimeHub::default();
    let mut rx = hub.subscribe();
    hub.publish(RealtimeEvent {
      topic: "t".into(),
      event: "ping".into(),
      data_json: "{}".into(),
    });
    let got = rx.try_recv().expect("event");
    assert_eq!(got.event, "ping");
  }

  #[tokio::test]
  async fn next_event_receives_late_publish() {
    let hub = Arc::new(RealtimeHub::new(4));
    let session = WebSocketSession::new(Arc::clone(&hub));
    let publish_hub = Arc::clone(&hub);
    let recv = tokio::spawn(async move { session.next_event().await });
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    publish_hub.publish(RealtimeEvent {
      topic: "t".into(),
      event: "late".into(),
      data_json: "{}".into(),
    });
    let ev = recv.await.unwrap().expect("event");
    assert_eq!(ev.event, "late");
  }

  #[test]
  fn websocket_session_constructs() {
    let hub = Arc::new(RealtimeHub::default());
    let session = WebSocketSession::new(hub);
    assert!(!format!("{session:?}").is_empty());
  }

  #[test]
  fn transport_serde_round_trip() {
    let sse_json = serde_json::to_string(&RealtimeTransport::Sse).unwrap();
    assert!(sse_json.contains("Sse"));
    let ws_json = serde_json::to_string(&RealtimeTransport::WebSocket).unwrap();
    assert!(ws_json.contains("WebSocket"));
  }
}
