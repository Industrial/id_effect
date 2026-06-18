//! [`EventEnvelope`] — metadata shell around a typed payload with [`Schema`] bridging.

use crate::error::EventStoreError;
use id_effect::schema::serde_bridge::unknown_from_serde_json;
use id_effect::schema::{HasSchema, ParseError, Schema, i64, string, struct4};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};

/// Domain event with stream metadata.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventEnvelope<E> {
  /// Unique event id.
  pub event_id: String,
  /// Aggregate / stream id.
  pub stream_id: String,
  /// Monotonic version within the stream (1-based).
  pub version: u64,
  /// Unix epoch milliseconds.
  pub occurred_at_ms: i64,
  /// Event type name for routing and schema selection.
  pub event_type: String,
  /// Domain payload.
  pub payload: E,
}

/// Wire representation with JSON payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireEventEnvelope {
  /// Unique event id.
  pub event_id: String,
  /// Aggregate / stream id.
  pub stream_id: String,
  /// Monotonic version within the stream (1-based).
  pub version: u64,
  /// Unix epoch milliseconds.
  pub occurred_at_ms: i64,
  /// Event type name.
  pub event_type: String,
  /// Encoded payload.
  pub payload: Value,
}

impl<E> EventEnvelope<E> {
  /// Build a new envelope (assigns `event_id` and `occurred_at_ms`).
  pub fn new(
    stream_id: impl Into<String>,
    version: u64,
    event_type: impl Into<String>,
    payload: E,
  ) -> Self {
    Self {
      event_id: uuid::Uuid::new_v4().to_string(),
      stream_id: stream_id.into(),
      version,
      occurred_at_ms: now_ms(),
      event_type: event_type.into(),
      payload,
    }
  }
}

/// Encode using [`HasSchema`] for the payload marker type `S`.
pub fn envelope_to_wire<S>(env: &EventEnvelope<S::A>) -> Result<WireEventEnvelope, ParseError>
where
  S: HasSchema,
  <S as HasSchema>::A: Clone,
  <S as HasSchema>::I: Serialize,
{
  let encoded = S::schema().encode(env.payload.clone());
  let payload = serde_json::to_value(encoded).map_err(|e| ParseError::new("", e.to_string()))?;
  Ok(WireEventEnvelope {
    event_id: env.event_id.clone(),
    stream_id: env.stream_id.clone(),
    version: env.version,
    occurred_at_ms: env.occurred_at_ms,
    event_type: env.event_type.clone(),
    payload,
  })
}

/// Decode using [`HasSchema`] for the payload marker type `S`.
pub fn envelope_from_wire<S>(wire: WireEventEnvelope) -> Result<EventEnvelope<S::A>, ParseError>
where
  S: HasSchema,
{
  let unknown = unknown_from_serde_json(wire.payload);
  let payload = S::schema().decode_unknown(&unknown)?;
  Ok(EventEnvelope {
    event_id: wire.event_id,
    stream_id: wire.stream_id,
    version: wire.version,
    occurred_at_ms: wire.occurred_at_ms,
    event_type: wire.event_type,
    payload,
  })
}

/// Canonical [`Schema`] for [`EventEnvelope`] metadata + payload schema `payload_schema`.
pub fn envelope_schema<A, I, E>(
  payload_schema: Schema<A, I, E>,
) -> Schema<EventEnvelope<A>, WireEventEnvelope, E>
where
  E: id_effect::schema::EffectData + 'static,
  A: Clone + 'static,
  I: Serialize + 'static,
{
  let meta = struct4(
    "event_id",
    string::<E>(),
    "stream_id",
    string::<E>(),
    "version",
    i64::<E>(),
    "occurred_at_ms",
    i64::<E>(),
  );
  let payload_decode = payload_schema.clone();
  let payload_encode = payload_schema.clone();
  let payload_unknown = payload_schema;
  Schema::make(
    move |wire: WireEventEnvelope| {
      let (event_id, stream_id, version, occurred_at_ms) = meta.decode((
        wire.event_id.clone(),
        wire.stream_id.clone(),
        wire.version as i64,
        wire.occurred_at_ms,
      ))?;
      let payload =
        payload_decode.decode_unknown(&unknown_from_serde_json(wire.payload.clone()))?;
      Ok(EventEnvelope {
        event_id,
        stream_id,
        version: version as u64,
        occurred_at_ms,
        event_type: wire.event_type.clone(),
        payload,
      })
    },
    move |env: EventEnvelope<A>| {
      let wire_payload = payload_encode.encode(env.payload.clone());
      let payload = serde_json::to_value(wire_payload).unwrap_or(Value::Null);
      WireEventEnvelope {
        event_id: env.event_id,
        stream_id: env.stream_id,
        version: env.version,
        occurred_at_ms: env.occurred_at_ms,
        event_type: env.event_type,
        payload,
      }
    },
    move |unknown: &id_effect::Unknown| {
      let payload = payload_unknown.decode_unknown(unknown)?;
      Ok(EventEnvelope {
        event_id: String::new(),
        stream_id: String::new(),
        version: 0,
        occurred_at_ms: 0,
        event_type: String::new(),
        payload,
      })
    },
  )
}

/// Map [`ParseError`] into [`EventStoreError::Schema`].
pub fn schema_error(err: ParseError) -> EventStoreError {
  EventStoreError::Schema(err.message)
}

fn now_ms() -> i64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map(|d| d.as_millis().min(i64::MAX as u128) as i64)
    .unwrap_or(0)
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect::schema::{HasSchema, Schema, i64};

  struct CounterIncreased;

  impl HasSchema for CounterIncreased {
    type A = i64;
    type I = i64;
    type E = ();

    fn schema() -> Schema<Self::A, Self::I, Self::E> {
      i64::<()>()
    }
  }

  #[test]
  fn event_envelope_new_sets_fields() {
    let env = EventEnvelope::new("stream", 7, "Evt", 3_i64);
    assert_eq!(env.stream_id, "stream");
    assert_eq!(env.version, 7);
    assert_eq!(env.event_type, "Evt");
    assert_eq!(env.payload, 3);
    assert!(!env.event_id.is_empty());
    assert!(env.occurred_at_ms >= 0);
  }

  #[test]
  fn envelope_schema_round_trip() {
    let env = EventEnvelope::new("acct-1", 1, "CounterIncreased", 5_i64);
    let wire = envelope_to_wire::<CounterIncreased>(&env).expect("to_wire");
    let back = envelope_from_wire::<CounterIncreased>(wire).expect("from_wire");
    assert_eq!(back.payload, 5);
    assert_eq!(back.stream_id, "acct-1");
  }

  #[test]
  fn schema_error_maps_parse_error() {
    let err = ParseError::new("field", "bad");
    assert_eq!(schema_error(err).to_string(), "schema error: bad");
  }

  #[test]
  fn envelope_schema_decode_unknown_payload() {
    let schema = envelope_schema(CounterIncreased::schema());
    let unknown = id_effect::Unknown::I64(3);
    let env = schema.decode_unknown(&unknown).expect("decode_unknown");
    assert_eq!(env.payload, 3);
  }

  #[test]
  fn envelope_schema_encode_round_trip() {
    let schema = envelope_schema(CounterIncreased::schema());
    let env = EventEnvelope::new("s", 3, "CounterIncreased", 12_i64);
    let wire = schema.encode(env.clone());
    let decoded = schema.decode(wire).expect("decode");
    assert_eq!(decoded.payload, 12);
    assert_eq!(decoded.stream_id, "s");
  }

  #[test]
  fn composite_schema_decode_fails_on_bad_version() {
    let schema = envelope_schema(CounterIncreased::schema());
    let wire = WireEventEnvelope {
      event_id: "e".into(),
      stream_id: "s".into(),
      version: 0,
      occurred_at_ms: 0,
      event_type: "t".into(),
      payload: serde_json::json!(null),
    };
    assert!(schema.decode(wire).is_err());
  }

  #[test]
  fn envelope_from_wire_rejects_bad_payload() {
    let wire = WireEventEnvelope {
      event_id: "e".into(),
      stream_id: "s".into(),
      version: 1,
      occurred_at_ms: 0,
      event_type: "CounterIncreased".into(),
      payload: serde_json::json!("not-a-number"),
    };
    assert!(envelope_from_wire::<CounterIncreased>(wire).is_err());
  }

  #[test]
  fn composite_envelope_schema_validates() {
    let schema = envelope_schema(CounterIncreased::schema());
    let env = EventEnvelope::new("s", 2, "CounterIncreased", 9_i64);
    let wire = envelope_to_wire::<CounterIncreased>(&env).expect("wire");
    let decoded = schema.decode(wire).expect("decode");
    assert_eq!(decoded.payload, 9);
  }
}
