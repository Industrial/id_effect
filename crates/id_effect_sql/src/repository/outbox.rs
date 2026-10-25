//! The transactional outbox from entity hooks (feature `outbox`, ADR 0015 D12): an `after_*`
//! [`EntityHooks`](super::EntityHooks) hook that stages an event with the write's transaction, so
//! the event exists if and only if the write commits.
//!
//! - [`OutboxEvent`]: what to publish — a topic, an id, an optional key, text headers and a
//!   payload (the fields of an `id_effect_messaging` `Message`).
//! - [`OutboxSink`]: where an event is staged, given the write's transaction. [`OutboxTable`]
//!   inserts it into the `id_effect_outbox` table in the row format of `id_effect_messaging`'s
//!   `Outbox::stage`, so its `Relay` publishes it after the commit (at least once); any
//!   `Fn(&Arc<dyn SqlTransaction>, OutboxEvent) -> Effect<(), SqlError>` is a sink too, e.g. one
//!   calling `id_effect_messaging::Outbox::stage` itself.
//! - [`EntityOutbox`]: turns an event function into a hook —
//!   [`on_save`](EntityOutbox::on_save) for `after_insert` / `after_update`,
//!   [`on_delete`](EntityOutbox::on_delete) for `after_delete`. The function returns `None` to
//!   stage nothing for that write.
//!
//! `id_effect_messaging` itself depends on this crate, so the feature has no dependency on it:
//! the adapter writes the messaging crate's table format (create the table with
//! `id_effect_messaging::migrator()`).
//!
//! ```rust
//! use id_effect::Exit;
//! use id_effect_sql::repository::EntityHooks;
//! use id_effect_sql::repository::outbox::{EntityOutbox, OutboxEvent, OutboxTable};
//! use id_effect_sql::{
//!   Dialect, Entity, FromRow, Repository, SqlClient, SqlClientEnv, TestSqlClient, sql_client_env,
//! };
//!
//! #[derive(Clone, Debug, PartialEq, FromRow, Entity)]
//! #[entity(table = "orders")]
//! struct Order {
//!   #[entity(id)]
//!   id: i64,
//!   status: String,
//! }
//!
//! let outbox = EntityOutbox::new(OutboxTable::new(Dialect::Postgres));
//! let hooks = EntityHooks::<Order>::new().after_update(outbox.on_save(|order: &Order| {
//!   Some(OutboxEvent::json("orders.updated", &serde_json::json!({ "id": order.id })))
//! }));
//! let orders = Repository::<Order>::new(Dialect::Postgres).with_hooks(hooks);
//!
//! let db = TestSqlClient::new();
//! let _ = db.connect().block_on();
//! db.script_execute(r#"UPDATE "orders" SET "status" = $1 WHERE "id" = $2"#, 1);
//! let updated = orders
//!   .update::<SqlClientEnv, _>(Order { id: 1, status: "paid".into() })
//!   .provide_all(sql_client_env(db.clone()));
//! assert!(matches!(updated.block_on(), Exit::Success(_)));
//! let staged = &db.recorded_executes()[1].0;
//! assert!(staged.starts_with(r#"INSERT INTO "id_effect_outbox""#), "{staged}");
//! assert_eq!(db.commits(), 1);
//! ```

use std::collections::BTreeMap;
use std::collections::hash_map::RandomState;
use std::fmt;
use std::hash::{BuildHasher, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use id_effect::{Effect, fail, succeed};
use sea_query::{Expr, Query, Value};

use super::Entity;
use super::hooks::HookContext;
use crate::error::SqlError;
use crate::query_builder::{BuildSql, Dialect};
use crate::transaction::SqlTransaction;
use crate::value::SqlValue;

/// The table `id_effect_messaging` stages messages in and relays them from.
pub const OUTBOX_TABLE: &str = "id_effect_outbox";

/// The header naming the payload's media type (`id_effect_messaging::headers::CONTENT_TYPE`).
pub const CONTENT_TYPE: &str = "content-type";

/// The [`CONTENT_TYPE`] of a JSON payload ([`OutboxEvent::json`]).
pub const JSON: &str = "application/json";

/// The longest topic, in bytes (`id_effect_messaging::Topic::MAX_LEN`).
pub const MAX_TOPIC_LEN: usize = 249;

/// An event to stage: a topic, an id (generated unless set), an optional key, text headers and
/// an opaque payload.
#[derive(Clone, PartialEq, Eq)]
pub struct OutboxEvent {
  id: String,
  topic: String,
  key: Option<String>,
  headers: BTreeMap<String, String>,
  payload: Vec<u8>,
}

impl fmt::Debug for OutboxEvent {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("OutboxEvent")
      .field("id", &self.id)
      .field("topic", &self.topic)
      .field("key", &self.key)
      .field("headers", &self.headers)
      .field("payload_len", &self.payload.len())
      .finish()
  }
}

/// A fresh id: 32 hex digits, the wall-clock nanoseconds then 64 random bits (the format of
/// `id_effect_messaging::MessageId::generate`).
fn generate_id() -> String {
  static COUNTER: AtomicU64 = AtomicU64::new(0);
  let nanos = SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX));
  let mut random = RandomState::new().build_hasher();
  random.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
  format!("{nanos:016x}{:016x}", random.finish())
}

impl OutboxEvent {
  /// An event for `topic` carrying `payload`, with a generated id.
  pub fn new(topic: impl Into<String>, payload: impl Into<Vec<u8>>) -> Self {
    Self {
      id: generate_id(),
      topic: topic.into(),
      key: None,
      headers: BTreeMap::new(),
      payload: payload.into(),
    }
  }

  /// An event for `topic` carrying `value` as JSON, with [`CONTENT_TYPE`] set to [`JSON`].
  pub fn json(topic: impl Into<String>, value: &serde_json::Value) -> Self {
    Self::new(topic, value.to_string()).with_header(CONTENT_TYPE, JSON)
  }

  /// With the id `id` instead of the generated one (an idempotency key consumers deduplicate
  /// on).
  #[must_use]
  pub fn with_id(mut self, id: impl Into<String>) -> Self {
    self.id = id.into();
    self
  }

  /// With the key `key` (the partition or ordering key on brokers that have one).
  #[must_use]
  pub fn with_key(mut self, key: impl Into<String>) -> Self {
    self.key = Some(key.into());
    self
  }

  /// With the header `name` set to `value` (replacing an earlier value).
  #[must_use]
  pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
    self.headers.insert(name.into(), value.into());
    self
  }

  /// The id.
  pub fn id(&self) -> &str {
    &self.id
  }

  /// The topic.
  pub fn topic(&self) -> &str {
    &self.topic
  }

  /// The key, if any.
  pub fn key(&self) -> Option<&str> {
    self.key.as_deref()
  }

  /// The headers.
  pub fn headers(&self) -> &BTreeMap<String, String> {
    &self.headers
  }

  /// The payload.
  pub fn payload(&self) -> &[u8] {
    &self.payload
  }

  /// The topic is a valid `id_effect_messaging` topic: 1 to [`MAX_TOPIC_LEN`] bytes of ASCII
  /// letters, digits, `.`, `_` and `-` (a relay could not publish the row otherwise).
  fn check_topic(&self) -> Result<(), SqlError> {
    let topic = &self.topic;
    let valid = !topic.is_empty()
      && topic.len() <= MAX_TOPIC_LEN
      && topic
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if valid {
      Ok(())
    } else {
      Err(SqlError::Unsupported(format!(
        "invalid outbox topic `{topic}` (expected 1 to {MAX_TOPIC_LEN} bytes of [A-Za-z0-9._-])"
      )))
    }
  }
}

/// Where an [`EntityOutbox`] stages events: with the write's transaction, so the event commits
/// or rolls back with the write.
pub trait OutboxSink: Send + Sync + 'static {
  /// Stage `event` with `tx`.
  fn stage(&self, tx: &Arc<dyn SqlTransaction>, event: OutboxEvent) -> Effect<(), SqlError>;
}

impl<F> OutboxSink for F
where
  F: Fn(&Arc<dyn SqlTransaction>, OutboxEvent) -> Effect<(), SqlError> + Send + Sync + 'static,
{
  fn stage(&self, tx: &Arc<dyn SqlTransaction>, event: OutboxEvent) -> Effect<(), SqlError> {
    self(tx, event)
  }
}

/// [`OutboxSink`] inserting into the outbox table ([`OUTBOX_TABLE`] unless
/// [`with_table`](Self::with_table)) in `id_effect_messaging`'s row format: `topic`,
/// `message_id`, `message_key`, `headers` (a JSON object of strings), `payload` (bytes) and
/// `available_at` = `inserted_at` = now. The statement is rendered for the dialect and bound,
/// never spliced; the topic is checked first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutboxTable {
  dialect: Dialect,
  table: &'static str,
}

impl OutboxTable {
  /// The table [`OUTBOX_TABLE`], statements in `dialect`.
  pub fn new(dialect: Dialect) -> Self {
    Self {
      dialect,
      table: OUTBOX_TABLE,
    }
  }

  /// Insert into `table` instead (same columns).
  #[must_use]
  pub fn with_table(mut self, table: &'static str) -> Self {
    self.table = table;
    self
  }

  /// The dialect statements are rendered in.
  pub fn dialect(&self) -> Dialect {
    self.dialect
  }

  /// The table events are inserted into.
  pub fn table(&self) -> &'static str {
    self.table
  }

  /// The `INSERT` staging `event` at `now`.
  fn statement(
    &self,
    event: OutboxEvent,
    now: jiff::Timestamp,
  ) -> Result<(String, Vec<SqlValue>), SqlError> {
    event.check_topic()?;
    let headers = serde_json::Value::Object(
      event
        .headers
        .into_iter()
        .map(|(k, v)| (k, serde_json::Value::String(v)))
        .collect(),
    );
    let key = event.key.map_or(SqlValue::Null, SqlValue::Text);
    let values = [
      SqlValue::Text(event.topic),
      SqlValue::Text(event.id),
      key,
      SqlValue::Json(headers),
      SqlValue::Bytes(event.payload),
      SqlValue::Timestamp(now),
      SqlValue::Timestamp(now),
    ];
    let stmt = Query::insert()
      .into_table(self.table)
      .columns([
        "topic",
        "message_id",
        "message_key",
        "headers",
        "payload",
        "available_at",
        "inserted_at",
      ])
      .values_panic(values.map(|v| Expr::val(Value::from(v))))
      .to_owned();
    stmt.build_sql(self.dialect)
  }
}

impl OutboxSink for OutboxTable {
  fn stage(&self, tx: &Arc<dyn SqlTransaction>, event: OutboxEvent) -> Effect<(), SqlError> {
    match self.statement(event, jiff::Timestamp::now()) {
      Ok((sql, params)) => tx.execute(&sql, &params).map(|_| ()),
      Err(e) => fail(e),
    }
  }
}

/// Builds `after_*` hooks staging events in an [`OutboxSink`] (see the [module docs](self)).
/// Cloning shares the sink.
pub struct EntityOutbox<S = OutboxTable> {
  sink: Arc<S>,
}

impl<S> Clone for EntityOutbox<S> {
  fn clone(&self) -> Self {
    Self {
      sink: Arc::clone(&self.sink),
    }
  }
}

impl<S: fmt::Debug> fmt::Debug for EntityOutbox<S> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_tuple("EntityOutbox").field(&self.sink).finish()
  }
}

impl EntityOutbox<OutboxTable> {
  /// Stage in [`OUTBOX_TABLE`] with statements in `dialect`
  /// (`EntityOutbox::new(OutboxTable::new(dialect))`).
  pub fn table(dialect: Dialect) -> Self {
    Self::new(OutboxTable::new(dialect))
  }
}

impl<S: OutboxSink> EntityOutbox<S> {
  /// Stage in `sink`.
  pub fn new(sink: S) -> Self {
    Self {
      sink: Arc::new(sink),
    }
  }

  /// The sink.
  pub fn sink(&self) -> &S {
    &self.sink
  }

  /// An `after_insert` / `after_update` hook staging `event(entity)` with the write's
  /// transaction (nothing when it returns `None`). A staging failure fails the hook, which rolls
  /// the write back.
  pub fn on_save<T, E, F>(
    &self,
    event: F,
  ) -> impl Fn(&HookContext<T>, &T) -> Effect<(), E> + Send + Sync + 'static
  where
    T: Entity,
    E: From<SqlError> + Send + 'static,
    F: Fn(&T) -> Option<OutboxEvent> + Send + Sync + 'static,
  {
    let sink = Arc::clone(&self.sink);
    move |ctx: &HookContext<T>, entity: &T| stage(&*sink, ctx, event(entity))
  }

  /// An `after_delete` (or `before_delete`) hook staging `event(id)` with the write's
  /// transaction (nothing when it returns `None`). A staging failure fails the hook, which rolls
  /// the delete back.
  pub fn on_delete<T, E, F>(
    &self,
    event: F,
  ) -> impl Fn(&HookContext<T>, &T::Id) -> Effect<(), E> + Send + Sync + 'static
  where
    T: Entity,
    E: From<SqlError> + Send + 'static,
    F: Fn(&T::Id) -> Option<OutboxEvent> + Send + Sync + 'static,
  {
    let sink = Arc::clone(&self.sink);
    move |ctx: &HookContext<T>, id: &T::Id| stage(&*sink, ctx, event(id))
  }
}

/// `event` staged in `sink` with `ctx`'s transaction, its error as `E`.
fn stage<S: OutboxSink, T, E>(
  sink: &S,
  ctx: &HookContext<T>,
  event: Option<OutboxEvent>,
) -> Effect<(), E>
where
  T: Entity,
  E: From<SqlError> + Send + 'static,
{
  match event {
    Some(event) => sink.stage(ctx.transaction(), event).map_err(E::from),
    None => succeed(()),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn statement_binds_every_column_and_checks_the_topic() {
    let table = OutboxTable::new(Dialect::Sqlite).with_table("events");
    assert_eq!(
      (table.dialect(), table.table()),
      (Dialect::Sqlite, "events")
    );
    let now = jiff::Timestamp::UNIX_EPOCH;
    let event = OutboxEvent::new("orders.paid", b"x".to_vec())
      .with_id("id-1")
      .with_key("k")
      .with_header("a", "b");
    let (sql, params) = table.statement(event, now).expect("statement");
    assert!(sql.starts_with(r#"INSERT INTO "events""#), "{sql}");
    assert_eq!(params.len(), 7);
    assert_eq!(params[1], SqlValue::Text("id-1".into()));
    assert_eq!(params[2], SqlValue::Text("k".into()));
    assert_eq!(params[3], SqlValue::Json(serde_json::json!({ "a": "b" })));
    let no_key = table
      .statement(OutboxEvent::new("t", Vec::new()), now)
      .expect("statement");
    assert_eq!(no_key.1[2], SqlValue::Null);

    for bad in ["", "with space", &"t".repeat(MAX_TOPIC_LEN + 1)] {
      let err = table
        .statement(OutboxEvent::new(bad, Vec::new()), now)
        .expect_err(bad);
      assert!(matches!(err, SqlError::Unsupported(_)), "{err:?}");
    }
  }

  #[test]
  fn events_carry_ids_headers_and_json() {
    let (a, b) = (
      OutboxEvent::new("t", vec![1]),
      OutboxEvent::new("t", vec![1]),
    );
    assert_ne!(a.id(), b.id());
    assert_eq!(a.id().len(), 32);
    let json = OutboxEvent::json("t", &serde_json::json!({ "n": 1 }));
    assert_eq!(json.payload(), br#"{"n":1}"#);
    assert_eq!(
      json.headers().get(CONTENT_TYPE).map(String::as_str),
      Some(JSON)
    );
    assert_eq!((json.topic(), json.key()), ("t", None));
    let debug = format!("{json:?}");
    assert!(debug.contains("payload_len: 7"), "{debug}");
    let outbox = EntityOutbox::table(Dialect::Postgres);
    assert_eq!(outbox.sink().dialect(), Dialect::Postgres);
    assert!(format!("{:?}", outbox.clone()).starts_with("EntityOutbox"));
  }
}
