//! Pub/sub: [`RedisClient::publish`], and subscriptions as an `id_effect` [`Stream`] of
//! [`RedisMessage`]s.

use std::sync::{Arc, Mutex, PoisonError};

use futures_util::StreamExt;
use id_effect::{Effect, Stream, from_async};
use redis::aio::PubSubStream;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::RedisError;
use crate::client::RedisClient;

/// One message received on a subscription.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RedisMessage {
  channel: String,
  pattern: Option<String>,
  payload: Vec<u8>,
}

impl RedisMessage {
  /// A message as a subscription delivers it (for tests and fakes).
  pub fn new(
    channel: impl Into<String>,
    pattern: Option<String>,
    payload: impl Into<Vec<u8>>,
  ) -> Self {
    Self {
      channel: channel.into(),
      pattern,
      payload: payload.into(),
    }
  }

  /// The channel it was published on.
  pub fn channel(&self) -> &str {
    &self.channel
  }

  /// The pattern that matched the channel (pattern subscriptions only).
  pub fn pattern(&self) -> Option<&str> {
    self.pattern.as_deref()
  }

  /// The raw payload.
  pub fn payload(&self) -> &[u8] {
    &self.payload
  }

  /// The payload as UTF-8 text.
  ///
  /// # Errors
  ///
  /// [`RedisError::Decode`] when it is not UTF-8.
  pub fn text(&self) -> Result<&str, RedisError> {
    std::str::from_utf8(&self.payload).map_err(|e| RedisError::Decode(e.to_string()))
  }

  /// The payload decoded from JSON (what [`RedisClient::publish_json`] sends).
  ///
  /// # Errors
  ///
  /// [`RedisError::Decode`] when it is not a JSON `T`.
  pub fn json<T: DeserializeOwned>(&self) -> Result<T, RedisError> {
    Ok(serde_json::from_slice(&self.payload)?)
  }
}

impl From<&redis::Msg> for RedisMessage {
  fn from(msg: &redis::Msg) -> Self {
    Self {
      channel: msg.get_channel_name().to_owned(),
      pattern: msg.get_pattern::<Option<String>>().ok().flatten(),
      payload: msg.get_payload_bytes().to_vec(),
    }
  }
}

impl RedisClient {
  /// Publish `payload` on `channel`; how many subscribers received it.
  pub fn publish(&self, channel: &str, payload: impl Into<Vec<u8>>) -> Effect<u64, RedisError> {
    self.query::<u64>(
      redis::cmd("PUBLISH")
        .arg(channel)
        .arg(payload.into())
        .clone(),
    )
  }

  /// Publish `value` as JSON on `channel` (read it with [`RedisMessage::json`]).
  pub fn publish_json<T: Serialize>(&self, channel: &str, value: &T) -> Effect<u64, RedisError> {
    match serde_json::to_vec(value) {
      Ok(payload) => self.publish(channel, payload),
      Err(e) => id_effect::fail(e.into()),
    }
  }

  /// Subscribe to `channels` on a dedicated connection. The effect completes once the server
  /// confirmed every subscription, so a message published afterwards is delivered; the stream
  /// then yields each message as it arrives, until it is dropped (which unsubscribes) or the
  /// connection is lost (a [`RedisError::Connection`] failure; subscribe again to resume).
  ///
  /// ```no_run
  /// use id_effect::Effect;
  /// use id_effect_redis::{RedisClient, RedisError};
  ///
  /// fn first_two(redis: &RedisClient) -> Effect<Vec<String>, RedisError> {
  ///   redis.subscribe(["orders"]).flat_map(|messages| {
  ///     messages
  ///       .take(2)
  ///       .map(|m| m.text().unwrap_or_default().to_owned())
  ///       .run_collect()
  ///   })
  /// }
  /// ```
  pub fn subscribe<I, S>(&self, channels: I) -> Effect<Stream<RedisMessage, RedisError>, RedisError>
  where
    I: IntoIterator<Item = S>,
    S: Into<String>,
  {
    self.open_subscription(channels.into_iter().map(Into::into).collect(), false)
  }

  /// [`subscribe`](Self::subscribe) to every channel matching the glob `patterns` (`PSUBSCRIBE`);
  /// each message carries the [`pattern`](RedisMessage::pattern) that matched.
  pub fn psubscribe<I, S>(
    &self,
    patterns: I,
  ) -> Effect<Stream<RedisMessage, RedisError>, RedisError>
  where
    I: IntoIterator<Item = S>,
    S: Into<String>,
  {
    self.open_subscription(patterns.into_iter().map(Into::into).collect(), true)
  }

  fn open_subscription(
    &self,
    names: Vec<String>,
    patterns: bool,
  ) -> Effect<Stream<RedisMessage, RedisError>, RedisError> {
    let client = self.redis().clone();
    from_async(move || async move {
      let mut pubsub = client.get_async_pubsub().await?;
      for name in &names {
        if patterns {
          pubsub.psubscribe(name).await?;
        } else {
          pubsub.subscribe(name).await?;
        }
      }
      Ok(message_stream(pubsub.into_on_message()))
    })
  }
}

type Slot = Arc<Mutex<Option<PubSubStream>>>;

fn lock(slot: &Slot) -> std::sync::MutexGuard<'_, Option<PubSubStream>> {
  slot.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One message per pull. A pull interrupted while waiting drops the connection, so the next
/// pull fails.
fn message_stream(messages: PubSubStream) -> Stream<RedisMessage, RedisError> {
  let slot: Slot = Arc::new(Mutex::new(Some(messages)));
  Stream::from_pull(move |_| {
    let slot = Arc::clone(&slot);
    from_async(move || async move {
      let taken = lock(&slot).take();
      let Some(mut messages) = taken else {
        return Err(RedisError::Connection(
          "the subscription was interrupted".into(),
        ));
      };
      match messages.next().await {
        Some(msg) => {
          *lock(&slot) = Some(messages);
          Ok(Some(vec![RedisMessage::from(&msg)]))
        }
        None => Err(RedisError::Connection(
          "the pub/sub connection closed".into(),
        )),
      }
    })
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn message_accessors() {
    let m = RedisMessage::new("orders", Some("ord*".into()), br#"{"id":7}"#.to_vec());
    assert_eq!(m.channel(), "orders");
    assert_eq!(m.pattern(), Some("ord*"));
    assert_eq!(m.text(), Ok(r#"{"id":7}"#));
    assert_eq!(m.payload(), br#"{"id":7}"#);
    let v: serde_json::Value = m.json().unwrap();
    assert_eq!(v["id"], 7);
    let bad = RedisMessage::new("c", None, vec![0xff]);
    assert!(matches!(bad.text(), Err(RedisError::Decode(_))));
    assert!(matches!(bad.json::<u32>(), Err(RedisError::Decode(_))));
    assert_eq!(bad.pattern(), None);
  }
}
