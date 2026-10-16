//! [`RoutingSqlClient`] — read/write splitting over a primary and its read replicas.
//!
//! The router is itself a [`SqlClient`], so it goes wherever a client goes
//! ([`sql_client_layer`](crate::sql_client_layer), [`with_transaction`](crate::with_transaction),
//! …). Reads ([`SqlClient::query`], [`SqlClient::query_stream`]) go to a replica chosen by its
//! [`ReadRouting`]; writes ([`SqlClient::execute`]) and transactions ([`SqlClient::begin`],
//! [`SqlClient::begin_with`]) go to the primary, so every statement of a transaction, reads
//! included, runs on the primary. Replicas lag their primary: a read that must see a write just
//! made (read-your-writes) belongs in the write's transaction or on
//! [`RoutingSqlClient::primary`].
//!
//! ```rust
//! use std::sync::Arc;
//! use id_effect_sql::{RoutingSqlClient, SqlClient, SqlRow, TestSqlClient};
//!
//! let (primary, replica) = (TestSqlClient::new(), TestSqlClient::new());
//! replica.script_query("SELECT n FROM t", vec![SqlRow::from_pairs([("n", 1i64)])]);
//! let db = RoutingSqlClient::new(Arc::new(primary.clone()), vec![Arc::new(replica.clone())]);
//! let _ = db.connect().block_on();
//! let _ = db.execute("INSERT INTO t VALUES (1)", &[]).block_on();
//! let _ = db.query("SELECT n FROM t", &[]).block_on();
//! assert_eq!((primary.recorded_executes().len(), primary.recorded_queries().len()), (1, 0));
//! assert_eq!(replica.recorded_queries().len(), 1);
//! ```

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use id_effect::{Effect, Never, Stream, succeed};

use crate::client::SqlClient;
use crate::error::SqlError;
use crate::row::SqlRow;
use crate::transaction::{SqlTransaction, TransactionOptions};
use crate::value::SqlValue;

/// Picks a replica index for a read: `(sql, replica_count) -> index` (taken modulo the count).
pub type ReplicaSelector = dyn Fn(&str, usize) -> usize + Send + Sync;

/// Which client a [`RoutingSqlClient`] read goes to.
#[derive(Clone, Default)]
#[non_exhaustive]
pub enum ReadRouting {
  /// Each read goes to the next replica in turn (the default).
  #[default]
  RoundRobin,
  /// Reads go to the primary too (routing switched off, e.g. by configuration).
  Primary,
  /// Each read goes to the replica a function picks from the statement and the replica count.
  Custom(Arc<ReplicaSelector>),
}

impl ReadRouting {
  /// [`ReadRouting::Custom`] from a function.
  pub fn custom(select: impl Fn(&str, usize) -> usize + Send + Sync + 'static) -> Self {
    Self::Custom(Arc::new(select))
  }
}

impl fmt::Debug for ReadRouting {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::RoundRobin => f.write_str("RoundRobin"),
      Self::Primary => f.write_str("Primary"),
      Self::Custom(_) => f.write_str("Custom(<fn>)"),
    }
  }
}

/// A [`SqlClient`] sending reads to read replicas and writes and transactions to the primary
/// (see the [module docs](self)). With no replicas every statement goes to the primary.
#[derive(Clone)]
pub struct RoutingSqlClient {
  primary: Arc<dyn SqlClient>,
  replicas: Arc<[Arc<dyn SqlClient>]>,
  routing: ReadRouting,
  next: Arc<AtomicUsize>,
}

impl RoutingSqlClient {
  /// Route reads round-robin over `replicas`, everything else to `primary`.
  pub fn new(primary: Arc<dyn SqlClient>, replicas: Vec<Arc<dyn SqlClient>>) -> Self {
    Self {
      primary,
      replicas: replicas.into(),
      routing: ReadRouting::RoundRobin,
      next: Arc::new(AtomicUsize::new(0)),
    }
  }

  /// Route reads with `routing` instead of round-robin.
  #[inline]
  #[must_use]
  pub fn with_read_routing(mut self, routing: ReadRouting) -> Self {
    self.routing = routing;
    self
  }

  /// The primary (for reads that must see the latest writes).
  #[inline]
  pub fn primary(&self) -> &Arc<dyn SqlClient> {
    &self.primary
  }

  /// The read replicas.
  #[inline]
  pub fn replicas(&self) -> &[Arc<dyn SqlClient>] {
    &self.replicas
  }

  /// The client the read `sql` goes to (advancing the round-robin).
  fn reader(&self, sql: &str) -> &Arc<dyn SqlClient> {
    let n = self.replicas.len();
    if n == 0 {
      return &self.primary;
    }
    match &self.routing {
      ReadRouting::RoundRobin => &self.replicas[self.next.fetch_add(1, Ordering::Relaxed) % n],
      ReadRouting::Primary => &self.primary,
      ReadRouting::Custom(select) => &self.replicas[select(sql, n) % n],
    }
  }

  /// The primary, then every replica.
  fn all(&self) -> impl Iterator<Item = &Arc<dyn SqlClient>> {
    std::iter::once(&self.primary).chain(self.replicas.iter())
  }
}

impl fmt::Debug for RoutingSqlClient {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("RoutingSqlClient")
      .field("replicas", &self.replicas.len())
      .field("routing", &self.routing)
      .finish_non_exhaustive()
  }
}

impl SqlClient for RoutingSqlClient {
  /// Connects the primary, then each replica; the first failure fails it.
  fn connect(&self) -> Effect<(), SqlError> {
    self
      .all()
      .fold(succeed(()), |done, client| done.then(client.connect()))
  }

  fn query(&self, sql: &str, params: &[SqlValue]) -> Effect<Vec<SqlRow>, SqlError> {
    self.reader(sql).query(sql, params)
  }

  fn execute(&self, sql: &str, params: &[SqlValue]) -> Effect<u64, SqlError> {
    self.primary.execute(sql, params)
  }

  fn begin(&self) -> Effect<Arc<dyn SqlTransaction>, SqlError> {
    self.primary.begin()
  }

  fn begin_with(&self, options: TransactionOptions) -> Effect<Arc<dyn SqlTransaction>, SqlError> {
    self.primary.begin_with(options)
  }

  fn query_stream(&self, sql: &str, params: &[SqlValue]) -> Stream<SqlRow, SqlError> {
    self.reader(sql).query_stream(sql, params)
  }

  /// Closes the primary and every replica.
  fn close(&self) -> Effect<(), Never> {
    self
      .all()
      .fold(succeed(()), |done, client| done.then(client.close()))
  }
}

#[cfg(test)]
mod tests {
  use std::sync::Arc;

  use super::*;
  use crate::client::TestSqlClient;
  use crate::client::tests::run;
  use crate::{Isolation, with_transaction};

  fn clients(n: usize) -> (TestSqlClient, Vec<TestSqlClient>, RoutingSqlClient) {
    let primary = TestSqlClient::new();
    let replicas: Vec<TestSqlClient> = (0..n).map(|_| TestSqlClient::new()).collect();
    let routed = RoutingSqlClient::new(
      Arc::new(primary.clone()),
      replicas
        .iter()
        .map(|r| Arc::new(r.clone()) as Arc<dyn SqlClient>)
        .collect(),
    );
    run(routed.connect()).expect("connect");
    (primary, replicas, routed)
  }

  fn reads(client: &TestSqlClient) -> Vec<String> {
    client
      .recorded_queries()
      .into_iter()
      .map(|(sql, _)| sql)
      .collect()
  }

  #[test]
  fn reads_go_to_replica_writes_to_primary() {
    let (primary, replicas, db) = clients(1);
    replicas[0].script_query("SELECT n FROM t", vec![SqlRow::from_pairs([("n", 7i64)])]);
    primary.script_execute("INSERT INTO t VALUES (1)", 1);
    assert_eq!(run(db.execute("INSERT INTO t VALUES (1)", &[])), Ok(1));
    let rows = run(db.query("SELECT n FROM t", &[])).expect("read");
    assert_eq!(rows[0].get::<i64>("n"), Ok(7));
    let streamed = run(db.query_stream("SELECT n FROM t", &[]).run_collect()).expect("stream");
    assert_eq!(streamed.len(), 1);
    assert_eq!(reads(&replicas[0]), ["SELECT n FROM t", "SELECT n FROM t"]);
    assert!(reads(&primary).is_empty());
    assert_eq!(primary.recorded_executes().len(), 1);
    assert!(replicas[0].recorded_executes().is_empty());
    // A transaction, and every statement in it, is the primary's.
    let client: Arc<dyn SqlClient> = Arc::new(db.clone());
    run(with_transaction(client, |tx| {
      tx.query("SELECT n FROM t FOR UPDATE", &[])
        .then(tx.execute("UPDATE t SET n = 2", &[]))
    }))
    .expect("transaction");
    let options = db.begin_with(Isolation::Serializable.into());
    run(options.flat_map(|tx| tx.commit())).expect("begin_with");
    assert_eq!((primary.commits(), replicas[0].commits()), (2, 0));
    assert_eq!(reads(&primary), ["SELECT n FROM t FOR UPDATE"]);
    assert_eq!(reads(&replicas[0]).len(), 2);
  }

  #[test]
  fn round_robin_spreads_reads_over_replicas() {
    let (primary, replicas, db) = clients(3);
    for _ in 0..7 {
      run(db.query("SELECT 1", &[])).expect("read");
    }
    let counts: Vec<usize> = replicas.iter().map(|r| reads(r).len()).collect();
    assert_eq!(counts, [3, 2, 2]);
    assert!(reads(&primary).is_empty());
    assert_eq!(db.replicas().len(), 3);
    run(db.primary().query("SELECT 1", &[])).expect("read on the primary");
    assert_eq!(reads(&primary).len(), 1);
  }

  #[test]
  fn other_routings_and_no_replicas() {
    let (primary, replicas, db) = clients(2);
    let db = db.with_read_routing(ReadRouting::Primary);
    run(db.query("SELECT 1", &[])).expect("read");
    assert_eq!(reads(&primary).len(), 1);
    let by_table = db.with_read_routing(ReadRouting::custom(|sql, n| {
      usize::from(sql.contains("audit")) % n
    }));
    run(by_table.query("SELECT * FROM audit", &[])).expect("read");
    run(by_table.query("SELECT * FROM users", &[])).expect("read");
    assert_eq!(
      (reads(&replicas[0]), reads(&replicas[1])),
      (
        vec!["SELECT * FROM users".to_owned()],
        vec!["SELECT * FROM audit".to_owned()]
      )
    );
    assert_eq!(format!("{:?}", ReadRouting::default()), "RoundRobin");
    assert_eq!(format!("{:?}", ReadRouting::Primary), "Primary");
    assert!(format!("{by_table:?}").contains("Custom(<fn>)"));
    assert!(format!("{by_table:?}").contains("replicas: 2"));

    let (primary, _, alone) = clients(0);
    run(alone.query("SELECT 1", &[])).expect("read");
    assert_eq!(reads(&primary).len(), 1);
  }

  #[test]
  fn connect_and_close_reach_every_client() {
    let (primary, replicas, db) = clients(2);
    assert!(primary.is_connected() && replicas.iter().all(TestSqlClient::is_connected));
    run(db.close().map_err(|n: Never| match n {})).expect("close");
    assert_eq!(primary.closes(), 1);
    assert!(replicas.iter().all(|r| r.closes() == 1));
    let failing = TestSqlClient::new();
    failing.fail_on_connect();
    let db = RoutingSqlClient::new(Arc::new(TestSqlClient::new()), vec![Arc::new(failing)]);
    assert_eq!(run(db.connect()), Err(SqlError::NotConnected));
  }
}
