//! The outbox, relay and inbox on PostgreSQL: staging rolls back with the caller's transaction,
//! two relays share the outbox with `SKIP LOCKED`, a crashed relay's row is published again, and
//! the inbox processes a redelivered message once.
//!
//! The tests need a database at `DATABASE_URL`. When CI provides a Postgres service it also sets
//! `ID_EFFECT_REQUIRE_PG=1`, which turns a missing or unreachable database into a test
//! **failure**; without it they print an explicit `SKIP` line to stderr and return. Each test runs
//! in a schema of its own (`search_path`), so parallel test processes never share rows.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use id_effect::{Effect, Exit, Schedule, UtcDateTime, WallClock, fail, succeed};
use id_effect_messaging::{
  BrokerError, ConsumerGroup, Inbox, MemoryBroker, Message, MessageBroker, MessageId, Once, Outbox,
  OutboxStore, PgOutbox, Relay, RelayConfig, SubscribeOptions, Topic, migrator,
};
use id_effect_sql::pg::{PgPoolConfig, PgSqlClient, pg_pool_from_config};
use id_effect_sql::{SqlClient, SqlError, SqlTransaction, SqlValue, params, with_transaction};
use id_effect_tokio::TokioRuntime;

const DOWN: &str = include_str!("../migrations/0001_id_effect_messaging.down.sql");

fn pg_required() -> bool {
  std::env::var("ID_EFFECT_REQUIRE_PG").is_ok_and(|v| v == "1")
}

/// Fail (when `ID_EFFECT_REQUIRE_PG=1`) or loudly skip a live test that has no database.
fn unavailable<T>(test: &str, why: &str) -> Option<T> {
  assert!(
    !pg_required(),
    "{test}: ID_EFFECT_REQUIRE_PG=1 but Postgres is unavailable: {why}"
  );
  eprintln!("SKIP {test}: {why} (set DATABASE_URL, and ID_EFFECT_REQUIRE_PG=1 to require it)");
  None
}

struct Db {
  rt: TokioRuntime,
  client: PgSqlClient,
}

impl Db {
  /// A client on the fresh, migrated schema `msg_<test>`.
  fn new(test: &str) -> Option<Self> {
    let url = match std::env::var("DATABASE_URL") {
      Ok(url) => url,
      Err(_) => return unavailable(test, "DATABASE_URL is not set"),
    };
    let rt = TokioRuntime::new_multi_thread().expect("tokio runtime");
    let schema = format!("msg_{test}");
    let admin = match rt.block_on(pg_pool_from_config(
      PgPoolConfig::from_url(url.clone()).with_max_size(1),
    )) {
      Ok(pool) => PgSqlClient::new(pool),
      Err(e) => return unavailable(test, &format!("cannot connect: {}", e.0)),
    };
    let fresh = admin
      .execute(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE"), &[])
      .flat_map(move |_| admin.execute(&format!("CREATE SCHEMA {schema}"), &[]));
    if let Exit::Failure(c) = rt.block_on(rt.run(fresh)) {
      return unavailable(test, &format!("{c:?}"));
    }
    let sep = if url.contains('?') { '&' } else { '?' };
    let scoped = format!("{url}{sep}options=-c%20search_path%3Dmsg_{test}");
    let pool = rt
      .block_on(pg_pool_from_config(
        PgPoolConfig::from_url(scoped).with_max_size(16),
      ))
      .expect("pool on the test schema");
    let db = Self {
      rt,
      client: PgSqlClient::new(pool),
    };
    let applied = db.run(
      migrator()
        .run_on(Arc::new(db.client.clone()))
        .map_err(|e| e.to_string()),
    );
    assert_eq!(applied, [1]);
    Some(db)
  }

  fn run<A: Send + 'static, E: std::fmt::Debug + Send + 'static>(&self, eff: Effect<A, E>) -> A {
    match self.rt.block_on(self.rt.run(eff)) {
      Exit::Success(a) => a,
      Exit::Failure(c) => panic!("failed: {c:?}"),
    }
  }

  fn texts(&self, sql: &str, params: &[SqlValue]) -> Vec<String> {
    self
      .run(self.client.query(sql, params))
      .iter()
      .map(|r| r.get::<String>(0usize).expect("a text column"))
      .collect()
  }

  fn count(&self, sql: &str) -> i64 {
    self.run(self.client.query(sql, &[]))[0]
      .get::<i64>(0usize)
      .expect("a count")
  }

  fn arc(&self) -> Arc<dyn SqlClient> {
    Arc::new(self.client.clone())
  }
}

fn orders() -> Topic {
  Topic::new("orders").unwrap()
}

fn group() -> ConsumerGroup {
  ConsumerGroup::new("billing").unwrap()
}

/// A clock anchored at 2030-01-01 on the runtime's executor clock.
fn clock(db: &Db) -> WallClock {
  WallClock::anchored(db.rt.now(), UtcDateTime::unsafe_make(1_893_456_000_000))
}

#[test]
fn outbox_row_rolls_back_with_tx() {
  let test = "outbox_row_rolls_back_with_tx";
  let Some(db) = Db::new(test) else {
    return;
  };
  db.run(
    db.client
      .execute("CREATE TABLE orders (id bigint PRIMARY KEY)", &[]),
  );
  let clock = clock(&db);
  let outbox = Outbox::new().with_clock(clock.clone());
  let count = |db: &Db| db.count("SELECT count(*) FROM id_effect_outbox");
  // The business row and the message in one transaction that fails: neither is kept.
  let failed = db.rt.block_on(db.rt.run(with_transaction(db.arc(), {
    let outbox = outbox.clone();
    move |tx: Arc<dyn SqlTransaction>| {
      let staged = outbox.stage(&tx, &orders(), Message::new("order 1").with_id("o-1"));
      tx.execute("INSERT INTO orders (id) VALUES (1)", &[])
        .then(staged)
        .flat_map(|_| fail::<(), _, _>(SqlError::Unsupported("the posting failed".into())))
    }
  })));
  assert!(matches!(failed, Exit::Failure(_)));
  assert_eq!(count(&db), 0, "rolled back with the transaction");
  assert_eq!(db.count("SELECT count(*) FROM orders"), 0);

  // Committed: the row, with the message's id, key, headers and payload, at the clock's time.
  let tx = db.run(db.client.begin());
  let message = Message::new("order 2")
    .with_id("o-2")
    .with_key("acct-7")
    .with_header("trace", "t-1");
  let id = db.run(outbox.stage(&tx, &orders(), message));
  assert_eq!(count(&db), 0, "not visible before the commit");
  db.run(tx.commit());
  assert_eq!(count(&db), 1);
  let row = db.texts(
    "SELECT concat_ws('|', topic, message_id, message_key, headers->>'trace', \
     convert_from(payload, 'UTF8'), to_char(inserted_at AT TIME ZONE 'UTC', 'YYYY-MM-DD'), \
     attempts) FROM id_effect_outbox WHERE id = $1",
    &params![id.0],
  );
  assert_eq!(row, ["orders|o-2|acct-7|t-1|order 2|2030-01-01|0"]);

  // The relay publishes it with the staged id.
  let broker = MemoryBroker::new();
  let relay = Relay::new(
    Arc::new(PgOutbox::new(db.client.clone())),
    Arc::new(broker.clone()),
  )
  .with_config(RelayConfig {
    clock,
    ..RelayConfig::default()
  });
  let report = db.run(relay.run_until_idle());
  assert_eq!((report.claimed, report.published), (1, 1));
  let published = broker.messages(&orders());
  assert_eq!(published.len(), 1);
  assert_eq!(published[0].id().as_str(), "o-2");
  assert_eq!(published[0].key(), Some("acct-7"));
  assert_eq!(published[0].header("trace"), Some("t-1"));
  assert_eq!(published[0].payload(), b"order 2");
  assert_eq!(
    db.count(
      "SELECT count(*) FROM id_effect_outbox WHERE sent_at IS NOT NULL AND locked_by IS NULL"
    ),
    1
  );
}

#[test]
fn relay_publishes_each_row_once_across_two_relays() {
  let test = "relay_publishes_each_row_once_across_two_relays";
  let Some(db) = Db::new(test) else {
    return;
  };
  let outbox = Outbox::new();
  for chunk in 0..10 {
    let tx = db.run(db.client.begin());
    for n in 0..20 {
      let id = format!("m-{}", chunk * 20 + n);
      db.run(outbox.stage(&tx, &orders(), Message::new(id.clone()).with_id(id)));
    }
    db.run(tx.commit());
  }
  let broker = MemoryBroker::new();
  let relays: Vec<_> = ["relay-a", "relay-b"]
    .into_iter()
    .map(|id| {
      let relay = Relay::new(
        Arc::new(PgOutbox::new(db.client.clone())),
        Arc::new(broker.clone()),
      )
      .with_config(RelayConfig {
        id: id.into(),
        batch: 7,
        poll_interval: Duration::from_millis(10),
        ..RelayConfig::default()
      });
      db.rt.fork(relay.run())
    })
    .collect();
  let deadline = Instant::now() + Duration::from_secs(60);
  let sent = loop {
    let sent = db.count("SELECT count(*) FROM id_effect_outbox WHERE sent_at IS NOT NULL");
    if sent == 200 || Instant::now() > deadline {
      break sent;
    }
    std::thread::sleep(Duration::from_millis(20));
  };
  for r in &relays {
    r.interrupt_now();
  }
  for r in relays {
    assert!(matches!(db.rt.block_on(r.join_future()), Exit::Failure(_)));
  }
  assert_eq!(sent, 200, "every row sent");
  let mut published: HashMap<String, usize> = HashMap::new();
  for m in broker.messages(&orders()) {
    *published.entry(m.id().to_string()).or_default() += 1;
  }
  assert_eq!(published.len(), 200);
  assert!(
    published.values().all(|&n| n == 1),
    "no row published twice: {:?}",
    published
      .iter()
      .filter(|(_, n)| **n > 1)
      .collect::<Vec<_>>()
  );
  assert_eq!(
    db.count("SELECT count(*) FROM id_effect_outbox WHERE attempts <> 1 OR locked_by IS NOT NULL"),
    0,
    "one attempt each, no lease left"
  );
}

#[test]
fn crashed_relay_row_is_republished() {
  let test = "crashed_relay_row_is_republished";
  let Some(db) = Db::new(test) else {
    return;
  };
  let clock = clock(&db);
  let outbox = Outbox::new().with_clock(clock.clone());
  let tx = db.run(db.client.begin());
  db.run(outbox.stage(&tx, &orders(), Message::new("a").with_id("m-a")));
  db.run(tx.commit());
  let store = PgOutbox::new(db.client.clone());
  let lease = Duration::from_secs(30);
  // A relay claims the row and dies before marking it sent (it may have published it).
  let now = db.run(clock.now::<SqlError, id_effect::HNil>());
  let crashed = db.run(store.claim("crashed", 10, now, lease));
  assert_eq!(crashed.len(), 1);
  let broker = MemoryBroker::new();
  let relay =
    Relay::new(Arc::new(store.clone()), Arc::new(broker.clone())).with_config(RelayConfig {
      id: "survivor".into(),
      lease,
      clock: clock.clone(),
      ..RelayConfig::default()
    });
  assert_eq!(
    db.run(relay.run_until_idle()).claimed,
    0,
    "leased to the crashed relay"
  );
  clock.jump(lease + Duration::from_secs(1));
  let report = db.run(relay.run_until_idle());
  assert_eq!((report.claimed, report.published), (1, 1));
  assert_eq!(broker.messages(&orders())[0].id().as_str(), "m-a");
  // The crashed relay's late settlement is fenced off.
  assert!(!db.run(store.mark_sent(crashed[0].id, "crashed", now)));
  let row = db.texts(
    "SELECT attempts || '|' || (sent_at IS NOT NULL) FROM id_effect_outbox",
    &[],
  );
  assert_eq!(row, ["2|true"], "the second attempt published it");
}

#[test]
fn relay_retries_a_broker_outage() {
  let test = "relay_retries_a_broker_outage";
  let Some(db) = Db::new(test) else {
    return;
  };
  let clock = clock(&db);
  let outbox = Outbox::new().with_clock(clock.clone());
  let tx = db.run(db.client.begin());
  for n in 0..3 {
    db.run(outbox.stage(&tx, &orders(), Message::new(format!("{n}"))));
  }
  db.run(tx.commit());
  let broker = MemoryBroker::new();
  let retry: Schedule<(), BrokerError> = Schedule::exponential(Duration::from_secs(5)).map(|_| ());
  let relay = Relay::new(
    Arc::new(PgOutbox::new(db.client.clone())),
    Arc::new(broker.clone()),
  )
  .with_config(RelayConfig {
    clock: clock.clone(),
    retry,
    ..RelayConfig::default()
  });
  broker.fail_with(Some("down"));
  let report = db.run(relay.run_until_idle());
  assert_eq!((report.retrying, report.released), (1, 2));
  let errors = db.texts(
    "SELECT last_error FROM id_effect_outbox WHERE last_error IS NOT NULL",
    &[],
  );
  assert_eq!(errors, ["broker connection: down"]);
  broker.fail_with(None);
  assert_eq!(
    db.run(relay.run_until_idle()).published,
    2,
    "the given-back rows"
  );
  clock.jump(Duration::from_secs(5));
  assert_eq!(
    db.run(relay.run_until_idle()).published,
    1,
    "the retried row"
  );
  assert_eq!(
    db.count("SELECT count(*) FROM id_effect_outbox WHERE sent_at IS NULL"),
    0
  );
  // Pruning deletes sent rows only.
  let store = PgOutbox::new(db.client.clone());
  let later = UtcDateTime::unsafe_make(1_893_456_000_000 + 3_600_000);
  assert_eq!(db.run(store.prune(later)), 3);
}

#[test]
fn inbox_skips_duplicate_delivery() {
  let test = "inbox_skips_duplicate_delivery";
  let Some(db) = Db::new(test) else {
    return;
  };
  db.run(
    db.client
      .execute("CREATE TABLE effects (message_id text PRIMARY KEY)", &[]),
  );
  let broker = MemoryBroker::new();
  // m-1 twice (a relay that published it and crashed before marking it sent), then m-2, whose
  // first attempt writes its effect and fails: rolled back with the inbox row, then redelivered.
  let m1 = Message::new("a").with_id("m-1");
  for m in [m1.clone(), m1, Message::new("b").with_id("m-2")] {
    db.run(broker.publish(&orders(), m));
  }
  let runs = Arc::new(AtomicUsize::new(0));
  let handler = Inbox::new().handler(db.arc(), {
    let runs = Arc::clone(&runs);
    move |tx: Arc<dyn SqlTransaction>, msg: &Message| {
      let n = runs.fetch_add(1, Ordering::SeqCst);
      let effect = tx
        .execute(
          "INSERT INTO effects (message_id) VALUES ($1)",
          &params![msg.id().as_str()],
        )
        .map(|_| ());
      if n == 1 {
        effect.flat_map(|()| fail(SqlError::Unsupported("first attempt fails".into())))
      } else {
        effect
      }
    }
  });
  let consumed = broker
    .subscribe(&orders(), &group(), SubscribeOptions::default().prefetch(1))
    .flat_map(move |deliveries| deliveries.take(4).run_for_each(handler));
  db.run(consumed);
  assert_eq!(runs.load(Ordering::SeqCst), 3, "m-1 once, m-2 twice");
  assert_eq!(
    db.texts("SELECT message_id FROM effects ORDER BY message_id", &[]),
    ["m-1", "m-2"]
  );
  assert_eq!(
    db.texts(
      "SELECT consumer_group || ':' || message_id FROM id_effect_inbox ORDER BY message_id",
      &[]
    ),
    ["billing:m-1", "billing:m-2"]
  );
  assert_eq!(
    broker.backlog(&orders(), &group()),
    0,
    "every delivery acked"
  );
  assert_eq!(broker.in_flight(&orders(), &group()), 0);

  // `once` directly: another group processes the same message; a duplicate leaves the
  // transaction usable.
  let inbox = Inbox::new();
  let tx = db.run(db.client.begin());
  let audit = ConsumerGroup::new("audit").unwrap();
  let id = MessageId::new("m-1");
  let first = db.run(inbox.once(&tx, &audit, &id, |_| succeed::<_, SqlError, _>(1)));
  let again = db.run(inbox.once(&tx, &audit, &id, |_| succeed::<_, SqlError, _>(2)));
  assert_eq!((first, again), (Once::Ran(1), Once::Duplicate));
  let usable = db.run(tx.query("SELECT 1::bigint AS one", &[]));
  assert_eq!(usable[0].get::<i64>("one"), Ok(1));
  db.run(tx.commit());
  assert_eq!(db.count("SELECT count(*) FROM id_effect_inbox"), 3);
  assert_eq!(db.run(inbox.prune(&db.arc(), Duration::ZERO)), 3);
}

#[test]
fn migrator_and_ensure_schema_agree() {
  let test = "migrator_and_ensure_schema_agree";
  let Some(db) = Db::new(test) else {
    return;
  };
  let store = PgOutbox::new(db.client.clone());
  db.run(store.ensure_schema());
  let tracked = db.count("SELECT count(*) FROM _id_effect_messaging_migrations WHERE version = 1");
  assert_eq!(tracked, 1, "tracked in the crate's own table");
  let columns = |table: &str| {
    db.texts(
      "SELECT column_name || ':' || data_type FROM information_schema.columns \
       WHERE table_schema = current_schema() AND table_name = $1 ORDER BY ordinal_position",
      &params![table],
    )
  };
  let outbox = columns("id_effect_outbox");
  assert_eq!(outbox.len(), 14);
  assert!(outbox.contains(&"payload:bytea".to_owned()));
  assert!(outbox.contains(&"headers:jsonb".to_owned()));
  let inbox = columns("id_effect_inbox");
  assert_eq!(inbox.len(), 3);
  let indexes = || {
    db.texts(
      "SELECT indexname::text FROM pg_indexes WHERE schemaname = current_schema() \
       AND tablename IN ('id_effect_outbox', 'id_effect_inbox') ORDER BY indexname",
      &[],
    )
  };
  let expected = [
    "id_effect_inbox_pkey",
    "id_effect_inbox_processed_idx",
    "id_effect_outbox_pending_idx",
    "id_effect_outbox_pkey",
    "id_effect_outbox_sent_idx",
  ];
  assert_eq!(indexes(), expected);
  // The down script drops both; `ensure_schema` alone recreates the same tables.
  db.run(db.client.execute(DOWN.trim(), &[]));
  assert!(indexes().is_empty());
  db.run(store.ensure_schema());
  assert_eq!(columns("id_effect_outbox"), outbox);
  assert_eq!(columns("id_effect_inbox"), inbox);
  assert_eq!(indexes(), expected);
}
