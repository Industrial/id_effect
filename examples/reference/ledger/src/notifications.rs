//! Balance notifications: every posting is announced on `ledger.entry-posted` through the
//! transactional outbox, and a consumer group turns each announcement into one balance
//! notification per account.
//!
//! 1. [`stage`]: the posting's `SERIALIZABLE` transaction writes an [`EntryPosted`] to
//!    `id_effect_outbox` ([`Outbox::stage`]) next to the entry, so the announcement exists if and
//!    only if the entry committed (a refused batch entry rolls its savepoint back, announcement
//!    included);
//! 2. [`relay`]: a [`Relay`] claims the committed rows (`FOR UPDATE SKIP LOCKED`, so every
//!    instance may run one), publishes them to the broker (Redis Streams in production) with
//!    the message id `entry-<id>`, and marks them sent;
//! 3. [`handler`]: the consumer group [`MessagingConfig::group`] records each message in
//!    `id_effect_inbox` in the same transaction as its `balance_notifications` rows
//!    ([`Inbox::handler`]), so a message delivered twice (a relay that crashed after publishing,
//!    a lost ack) notifies once. A message it cannot process is requeued and, after
//!    `max_deliveries`, moved to the dead-letter stream `ledger.entry-posted.dlq`.

use std::sync::{Arc, LazyLock};

use id_effect::{Effect, fail, succeed};
use id_effect_kit::messaging::{
  BrokerError, ConsumerGroup, Delivery, Inbox, Message, MessageBroker, MessageBrokerExt, Outbox,
  OutboxId, PgOutbox, Relay, SubscribeOptions, Topic,
};
use id_effect_kit::sql::{SqlClient, SqlClientService, SqlError, SqlTransaction, params};

use crate::config::MessagingConfig;
use crate::error::StartupError;

/// What a posting announces.
#[derive(Clone, Debug, PartialEq, id_effect::Schema)]
pub struct EntryPosted {
  /// The organisation.
  pub org_id: String,
  /// The entry.
  pub entry_id: i64,
  /// The accounts it touched.
  pub account_ids: Vec<i64>,
}

/// The topic and the group of `config`.
pub fn names(config: &MessagingConfig) -> Result<(Topic, ConsumerGroup), StartupError> {
  let topic = Topic::new(config.topic.clone())
    .map_err(|e| StartupError::Settings(format!("messaging.topic: {e}")))?;
  let group = ConsumerGroup::new(config.group.clone())
    .map_err(|e| StartupError::Settings(format!("messaging.group: {e}")))?;
  Ok((topic, group))
}

/// Writes `posted` to the outbox with the posting's transaction `tx`.
pub fn stage(
  tx: &Arc<dyn SqlTransaction>,
  topic: &Topic,
  posted: EntryPosted,
) -> Effect<OutboxId, SqlError> {
  let message = Message::encode(posted.clone())
    .with_id(format!("entry-{}", posted.entry_id))
    .with_key(posted.org_id);
  Outbox::new().stage(tx, topic, message)
}

/// The relay from the outbox in `db` to `broker`.
pub fn relay(db: &Arc<dyn SqlClient>, broker: Arc<dyn MessageBroker>) -> Relay {
  Relay::new(
    Arc::new(PgOutbox::from_service(SqlClientService(Arc::clone(db)))),
    broker,
  )
}

/// How the consumer group reads: one message at a time, dead-lettered after
/// `config.max_deliveries` failed deliveries.
pub fn options(config: &MessagingConfig) -> SubscribeOptions {
  SubscribeOptions::default()
    .prefetch(8)
    .max_deliveries(config.max_deliveries)
}

/// Why a notification was not written (the delivery is requeued).
#[derive(Debug)]
pub enum NotifyError {
  /// The database refused.
  Sql(SqlError),
  /// The payload is not an [`EntryPosted`].
  Decode(BrokerError),
}

impl From<SqlError> for NotifyError {
  fn from(e: SqlError) -> Self {
    Self::Sql(e)
  }
}

const NOTIFY: &str = "INSERT INTO balance_notifications (org_id, account_id, entry_id, balance) \
  SELECT $1, $2, $3, COALESCE(SUM(amount), 0)::int8 FROM postings \
  WHERE org_id = $1 AND account_id = $2 \
  ON CONFLICT (entry_id, account_id) DO NOTHING";

/// The balance notifications of one message, in the inbox transaction `tx`.
pub fn notify(tx: Arc<dyn SqlTransaction>, message: &Message) -> Effect<(), NotifyError> {
  let posted = match message.decode::<EntryPosted>() {
    Ok(posted) => posted,
    Err(e) => return fail(NotifyError::Decode(e)),
  };
  posted
    .account_ids
    .iter()
    .fold(succeed(()), |done, account| {
      let insert = tx
        .execute(
          NOTIFY,
          &params![posted.org_id.as_str(), *account, posted.entry_id],
        )
        .map(|_| ())
        .map_err(NotifyError::from);
      done.flat_map(move |()| insert)
    })
}

/// The inbox of the consumers (on the system clock). A static: [`Inbox::handler`]'s handler
/// borrows the inbox it came from.
static INBOX: LazyLock<Inbox> = LazyLock::new(Inbox::new);

/// The consumer's delivery handler: [`notify`] once per message ([`Inbox::handler`]).
pub fn handler(
  db: Arc<dyn SqlClient>,
) -> impl Fn(Delivery) -> Effect<(), BrokerError> + Send + Sync + 'static {
  INBOX.handler(db, notify)
}

/// Consumes `topic` as `group` until the broker closes or fails.
pub fn consume(
  broker: Arc<dyn MessageBroker>,
  topic: Topic,
  group: ConsumerGroup,
  options: SubscribeOptions,
  db: Arc<dyn SqlClient>,
) -> Effect<(), BrokerError> {
  broker.consume(&topic, &group, options, handler(db))
}
