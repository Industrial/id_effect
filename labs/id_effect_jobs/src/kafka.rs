//! Kafka [`MessageBroker`](crate::MessageBroker) via [rdkafka](https://docs.rs/rdkafka).

use std::time::Duration;

use id_effect::Effect;
use rdkafka::config::ClientConfig;
use rdkafka::producer::{FutureProducer, FutureRecord};

use crate::broker::MessageBroker;
use crate::error::JobError;

/// rdkafka producer configuration.
#[derive(Clone, Debug)]
pub struct RdKafkaConfig {
  /// `bootstrap.servers` list.
  pub bootstrap_servers: String,
  /// Optional `message.timeout.ms` (default 5000).
  pub message_timeout_ms: Option<u64>,
  /// Enable idempotent producer (`enable.idempotence=true`, `acks=all`).
  pub enable_idempotence: bool,
}

impl RdKafkaConfig {
  /// Minimal producer targeting `bootstrap_servers`.
  pub fn new(bootstrap_servers: impl Into<String>) -> Self {
    Self {
      bootstrap_servers: bootstrap_servers.into(),
      message_timeout_ms: Some(5_000),
      enable_idempotence: true,
    }
  }
}

/// Production Kafka publisher.
#[derive(Clone)]
pub struct RdKafkaBroker {
  producer: FutureProducer,
}

impl RdKafkaBroker {
  /// Build a producer from `config`.
  pub fn new(config: RdKafkaConfig) -> Result<Self, JobError> {
    let mut client = ClientConfig::new();
    client.set("bootstrap.servers", &config.bootstrap_servers);
    if let Some(ms) = config.message_timeout_ms {
      client.set("message.timeout.ms", ms.to_string());
    }
    if config.enable_idempotence {
      client.set("enable.idempotence", "true");
      client.set("acks", "all");
    }
    let producer: FutureProducer = client
      .create()
      .map_err(|e| JobError::Storage(e.to_string()))?;
    Ok(Self { producer })
  }
}

impl MessageBroker for RdKafkaBroker {
  fn publish(&self, topic: &str, payload: &[u8]) -> Effect<(), JobError, ()> {
    let producer = self.producer.clone();
    let topic = topic.to_owned();
    let payload = payload.to_vec();
    Effect::new_async(move |_r| {
      Box::pin(async move {
        let record = FutureRecord::<(), _>::to(&topic).payload(&payload);
        producer
          .send(record, Duration::from_secs(5))
          .await
          .map_err(|(e, _)| JobError::Storage(e.to_string()))?;
        Ok(())
      })
    })
  }
}
