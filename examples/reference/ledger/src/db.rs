//! PostgreSQL: the primary, the read replicas behind a [`RoutingSqlClient`], and the migrations
//! the service applies when it starts.
//!
//! Every statement runs in a `db.query` span with the operation counted and timed
//! ([`InstrumentedSqlClient`]): under the OpenTelemetry span hook the spans are exported as
//! client spans of the request's trace, with the statement's shape and never its parameters.

use std::path::PathBuf;
use std::sync::Arc;

use id_effect::{Effect, Layer, Never, Stream, fail, from_async, hlist, need, succeed};
use id_effect_kit::sql::metrics::InstrumentedSqlClient;
use id_effect_kit::sql::migrate::{self, MigratedEnv, MigrationsApplied};
use id_effect_kit::sql::pg::{PgPoolConfig, PgSqlClient, pg_pool_from_config};
use id_effect_kit::sql::{
  FromRow, RoutingSqlClient, SqlClient, SqlClientService, SqlError, SqlRow, SqlTransaction,
  SqlValue, TransactionOptions,
};

use crate::config::DbConfig;
use crate::error::StartupError;

/// The databases of one service instance.
#[derive(Clone)]
pub struct Database {
  /// Every write, every transaction and every read that must see its own writes.
  pub primary: Arc<dyn SqlClient>,
  /// Reports and balances: the replicas round-robin, or the primary when there are none.
  pub reads: Arc<dyn SqlClient>,
}

impl Database {
  /// `primary`, with reads routed to `replicas` (to the primary when the list is empty); every
  /// one instrumented ([module docs](self)).
  pub fn new(primary: Arc<dyn SqlClient>, replicas: Vec<Arc<dyn SqlClient>>) -> Self {
    let primary = instrumented(primary);
    let replicas = replicas.into_iter().map(instrumented).collect();
    let reads: Arc<dyn SqlClient> = Arc::new(RoutingSqlClient::new(Arc::clone(&primary), replicas));
    Self { primary, reads }
  }

  /// Opens a pool to the primary and to each replica of `config`.
  pub fn connect(config: &DbConfig) -> Effect<Database, StartupError> {
    let max = config.max_connections;
    let primary = PgPoolConfig::from_url(config.url().expose()).with_max_size(max);
    let replicas: Vec<PgPoolConfig> = config
      .replicas
      .iter()
      .map(|url| PgPoolConfig::from_url(url.expose()).with_max_size(max))
      .collect();
    from_async(move || async move {
      let primary = open(primary).await?;
      let mut reads = Vec::with_capacity(replicas.len());
      for replica in replicas {
        reads.push(open(replica).await?);
      }
      Ok(Database::new(primary, reads))
    })
  }
}

/// `client` in `db.query` spans, counted and timed (`db.system = postgresql`).
fn instrumented(client: Arc<dyn SqlClient>) -> Arc<dyn SqlClient> {
  Arc::new(InstrumentedSqlClient::new(Shared(client)).with_db_system("postgresql"))
}

/// A shared client as a [`SqlClient`] value, for the wrappers generic over one.
#[derive(Clone)]
struct Shared(Arc<dyn SqlClient>);

impl SqlClient for Shared {
  fn connect(&self) -> Effect<(), SqlError> {
    self.0.connect()
  }

  fn query(&self, sql: &str, params: &[SqlValue]) -> Effect<Vec<SqlRow>, SqlError> {
    self.0.query(sql, params)
  }

  fn execute(&self, sql: &str, params: &[SqlValue]) -> Effect<u64, SqlError> {
    self.0.execute(sql, params)
  }

  fn begin(&self) -> Effect<Arc<dyn SqlTransaction>, SqlError> {
    self.0.begin()
  }

  fn begin_with(&self, options: TransactionOptions) -> Effect<Arc<dyn SqlTransaction>, SqlError> {
    self.0.begin_with(options)
  }

  fn query_stream(&self, sql: &str, params: &[SqlValue]) -> Stream<SqlRow, SqlError> {
    self.0.query_stream(sql, params)
  }

  fn close(&self) -> Effect<(), Never> {
    self.0.close()
  }
}

async fn open(config: PgPoolConfig) -> Result<Arc<dyn SqlClient>, StartupError> {
  let pool = pg_pool_from_config(config)
    .await
    .map_err(|e| StartupError::Sql(e.0))?;
  Ok(Arc::new(PgSqlClient::new(pool)))
}

/// Runs `sql` on `client` and decodes every row as `T`.
pub fn query_all<T: FromRow + Send + 'static>(
  client: &Arc<dyn SqlClient>,
  sql: &str,
  params: Vec<SqlValue>,
) -> Effect<Vec<T>, SqlError> {
  client
    .query(sql, &params)
    .flat_map(|rows| match rows.iter().map(T::from_row).collect() {
      Ok(all) => succeed(all),
      Err(e) => fail(e),
    })
}

/// Runs `sql` on `client` and decodes its first row as `T` ([`SqlError::RowNotFound`] without
/// one).
pub fn query_one<T: FromRow + Send + 'static>(
  client: &Arc<dyn SqlClient>,
  sql: &str,
  params: Vec<SqlValue>,
) -> Effect<T, SqlError> {
  query_all::<T>(client, sql, params).flat_map(|rows| match rows.into_iter().next() {
    Some(row) => succeed(row),
    None => fail(SqlError::RowNotFound),
  })
}

/// Applies the pending migrations of `dir` to `client` through the `migrate` layer (what
/// `id-effect migrate run` does): one transaction per migration under an advisory lock, so
/// instances starting together apply each one once. Returns the versions this call applied.
pub fn migrate(
  client: Arc<dyn SqlClient>,
  dir: impl Into<PathBuf>,
) -> Effect<Vec<i64>, StartupError> {
  let applied: Effect<Vec<i64>, StartupError, MigratedEnv> =
    need::<MigrationsApplied>().map(|applied| applied.0.clone());
  let database = Layer::succeed(hlist![SqlClientService(client)]);
  applied.provide_layer(migrate::layer::<StartupError>(dir).provide_merge(database))
}
