//! Message broker trait and Kafka adapter stub.

use id_effect::Effect;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use crate::error::JobError;

/// Publish/subscribe boundary for async messaging.
/// Publish messages to a topic.
pub trait MessageBroker: Send + Sync {
  /// Publish `payload` to `topic`.
  fn publish(&self, topic: &str, payload: &[u8]) -> Effect<(), JobError, ()>;
}

/// In-memory FIFO per topic (tests).
#[derive(Clone, Default)]
pub struct MemoryBroker {
  topics: Arc<Mutex<std::collections::HashMap<String, VecDeque<Vec<u8>>>>>,
}

impl MemoryBroker {
  /// Empty broker.
  pub fn new() -> Self {
    Self::default()
  }
  /// Drain queued messages for tests.
  pub fn drain_topic(&self, topic: &str) -> Vec<Vec<u8>> {
    self
      .topics
      .lock()
      .ok()
      .and_then(|mut g| g.remove(topic))
      .map(|q| q.into())
      .unwrap_or_default()
  }
}

impl MessageBroker for MemoryBroker {
  fn publish(&self, topic: &str, payload: &[u8]) -> Effect<(), JobError, ()> {
    let topic = topic.to_owned();
    let payload = payload.to_vec();
    let topics = Arc::clone(&self.topics);
    Effect::new(move |_r| {
      let mut guard = topics.lock().map_err(|e| JobError::Lock(e.to_string()))?;
      guard.entry(topic).or_default().push_back(payload);
      Ok(())
    })
  }
}

/// Kafka adapter stub — logs intent and delegates to memory queue until rdkafka wiring lands.
#[derive(Clone)]
pub struct KafkaBrokerStub {
  inner: MemoryBroker,
  bootstrap: String,
}

impl KafkaBrokerStub {
  /// Stub targeting `bootstrap_servers`.
  pub fn new(bootstrap_servers: impl Into<String>) -> Self {
    Self {
      inner: MemoryBroker::new(),
      bootstrap: bootstrap_servers.into(),
    }
  }

  /// Configured bootstrap list.
  pub fn bootstrap_servers(&self) -> &str {
    &self.bootstrap
  }
}

impl MessageBroker for KafkaBrokerStub {
  fn publish(&self, topic: &str, payload: &[u8]) -> Effect<(), JobError, ()> {
    tracing::debug!(bootstrap = %self.bootstrap, topic, bytes = payload.len(), "kafka stub publish");
    self.inner.publish(topic, payload)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect::run_blocking;

  #[test]
  fn kafka_stub_publishes_to_memory() {
    let broker = KafkaBrokerStub::new("localhost:9092");
    run_blocking(broker.publish("orders", b"{}"), ()).unwrap();
    assert_eq!(broker.inner.drain_topic("orders").len(), 1);
    assert_eq!(broker.bootstrap_servers(), "localhost:9092");
  }

  #[test]
  fn memory_broker_round_trip() {
    let broker = MemoryBroker::new();
    run_blocking(broker.publish("t", b"one"), ()).unwrap();
    run_blocking(broker.publish("t", b"two"), ()).unwrap();
    let drained = broker.drain_topic("t");
    assert_eq!(drained.len(), 2);
    assert_eq!(drained[0], b"one".to_vec());
    assert!(broker.drain_topic("t").is_empty());
  }
}
