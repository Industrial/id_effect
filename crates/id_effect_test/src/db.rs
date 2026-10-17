//! Per-test database isolation (feature `sql`): every test gets a database nobody else sees,
//! as a scoped [`Layer`] providing the `SqlClientService` the code under test already reads.
//!
//! | Mode | Layer | Isolation | Cost | Use when |
//! |------|-------|-----------|------|----------|
//! | rollback | [`isolated_db()`] over any client layer | one transaction, always rolled back | a `BEGIN` / `ROLLBACK` | the code under test does not need its writes committed |
//! | clone | [`TemplateDb::isolated_db`] over a [`PostgresContainer`] | a database of its own, cloned from a migrated template | a `CREATE DATABASE … TEMPLATE …` (milliseconds) | the code commits, opens its own connections, or tests concurrency |
//!
//! # Rollback mode
//!
//! [`isolated_db()`] needs a `SqlClientService` (a pool over a [`Shared`](crate::Shared)
//! PostgreSQL, a SQLite file, any [`SqlClient`]) and provides a client over **one transaction**
//! ([`TransactionSqlClient`]) plus that transaction as [`SqlTransactionService`]. Everything the
//! program runs goes through the transaction, and when the layer's scope closes it is rolled
//! back — after a success, a failure, a defect or an interrupt — so nothing the test wrote is
//! ever visible to another test, and the next test starts from the same rows.
//!
//! Inside it, code keeps working as against a real client:
//!
//! - each statement runs under a savepoint, so a failing statement (a constraint violation the
//!   test expects) undoes only itself and the transaction stays usable, as in autocommit mode;
//! - [`SqlClient::begin`] (and so [`transaction`](id_effect_sql::transaction),
//!   [`with_transaction`](id_effect_sql::with_transaction), `transaction_layer`) opens a
//!   savepoint: its commit releases it, its rollback rolls back to it. Isolation options
//!   ([`TransactionOptions`]) are ignored: the test transaction's level applies.
//!
//! ```rust
//! use id_effect::{Effect, Exit};
//! use id_effect_sql::{SqlClient, SqlClientService, SqlError, TestSqlClient, execute, provide_sql_client};
//! use id_effect_test::isolated_db;
//!
//! let db = TestSqlClient::new();
//! let _ = db.connect().block_on();
//! let insert: Effect<u64, SqlError, id_effect::caps!(SqlClientService)> =
//!   execute("INSERT INTO accounts VALUES (1)", vec![]);
//! let exit = insert.provide_layer(provide_sql_client(db.clone()).provide_to(isolated_db()));
//! assert!(matches!(exit.block_on(), Exit::Success(0)));
//! // The test transaction was rolled back: nothing was committed.
//! assert_eq!((db.commits(), db.rollbacks(), db.recorded_executes().len()), (0, 1, 0));
//! ```
//!
//! # Clone mode
//!
//! A [`TemplateDb`] is a `static` holding the setup of the schema (typically the app's
//! migrations, [`migrate`](fn@migrate) with feature `migrate`). The first test of a binary that
//! asks for it creates a template database on the server and runs the setup into it, once; every
//! test then gets `CREATE DATABASE <fresh> TEMPLATE <template>` — a copy of the migrated schema
//! in milliseconds — with a pool over it, and the database is dropped when the layer's scope
//! closes. The code under test commits, opens transactions and connections of its own (the
//! database's URL is in [`IsolatedDatabase`]) and never sees another test's data.
//!
//! ```no_run
//! use std::sync::Arc;
//! use id_effect::{Effect, Exit, caps};
//! use id_effect_sql::{SqlClient, SqlClientService, SqlError, execute};
//! use id_effect_test::{ContainerError, Postgres, Shared, TemplateDb};
//!
//! static PG: Shared<Postgres> = Shared::new(Postgres::new);
//! static SCHEMA: TemplateDb = TemplateDb::new(|db: Arc<dyn SqlClient>| {
//!   db.execute("CREATE TABLE accounts (id BIGINT PRIMARY KEY)", &[]).map(|_| ())
//! });
//!
//! #[derive(Debug)]
//! enum TestError {
//!   Container(ContainerError),
//!   Sql(SqlError),
//! }
//! impl From<ContainerError> for TestError {
//!   fn from(e: ContainerError) -> Self { Self::Container(e) }
//! }
//! impl From<SqlError> for TestError {
//!   fn from(e: SqlError) -> Self { Self::Sql(e) }
//! }
//!
//! let insert: Effect<u64, TestError, caps!(SqlClientService)> =
//!   execute("INSERT INTO accounts VALUES (1)", vec![]).map_err(TestError::from);
//! // Run on a tokio runtime (sqlx): `TokioRuntime::current()?.run(program).await`.
//! let program = insert.provide_layer(PG.layer().provide_to(SCHEMA.isolated_db()));
//! # let _ = program;
//! ```
//!
//! [`PostgresContainer`]: crate::PostgresContainer

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use id_effect::{
  Effect, ExitKind, HCons, HNil, Layer, Never, Semaphore, acquire_use_release, fail, from_async,
  hlist, need, succeed, suspend,
};
use id_effect_sql::pg::{PgPoolConfig, PgSqlClient, pg_pool_from_config};
use id_effect_sql::{
  SqlClient, SqlClientEnv, SqlClientService, SqlError, SqlRow, SqlTransaction,
  SqlTransactionService, SqlValue, TransactionOptions, savepoint,
};

use crate::containers::PostgresContainer;

/// What [`isolated_db()`] provides: a client over the test transaction, and the transaction.
pub type IsolatedDbEnv = HCons<SqlClientService, HCons<SqlTransactionService, HNil>>;

/// What [`TemplateDb::isolated_db`] provides: a client over the cloned database, and where it is.
pub type ClonedDbEnv = HCons<SqlClientService, HCons<IsolatedDatabase, HNil>>;

/// Counter making generated database and savepoint names unique within the process.
static NAMES: AtomicU64 = AtomicU64::new(0);

/// `<prefix>_<pid>_<n>`: unique per process, and across processes sharing a server.
fn unique_name(prefix: &str) -> String {
  let n = NAMES.fetch_add(1, Ordering::Relaxed);
  format!("{prefix}_{}_{n}", std::process::id())
}

fn ready<A: Send + 'static>(r: Result<A, SqlError>) -> Effect<A, SqlError> {
  match r {
    Ok(a) => succeed(a),
    Err(e) => fail(e),
  }
}

/// Runs `eff`, ignoring its outcome (cleanup that must not mask the original result).
fn quietly(eff: Effect<(), SqlError>) -> Effect<(), Never> {
  eff.exit::<Never>().map(|_| ())
}

/// A cleanup that cannot fail, in a pipeline that can.
fn widen(eff: Effect<(), Never>) -> Effect<(), SqlError> {
  eff.exit().map(|_| ())
}

// ---------------------------------------------------------------------------------------------
// Rollback mode
// ---------------------------------------------------------------------------------------------

/// Rollback-mode isolation: a scoped [`Layer`] that begins a transaction on the
/// `SqlClientService` it needs and provides a [`TransactionSqlClient`] over it (as
/// `SqlClientService`) and the transaction ([`SqlTransactionService`]). When the layer's scope
/// closes the transaction is **rolled back**, whatever the program's exit — success, failure,
/// defect or interrupt — so the test's writes are never seen by anyone else. See the
/// [module docs](self).
///
/// Wire it over the client layer with `provide_to`, so the program sees only the isolated
/// client: `program.provide_layer(client_layer.provide_to(isolated_db()))`, or
/// `program.provide_layer(isolated_db()).provide_layer(client_layer)`.
pub fn isolated_db<E>() -> Layer<IsolatedDbEnv, E, SqlClientEnv>
where
  E: From<SqlError> + Send + 'static,
{
  Layer::acquire_release(
    || {
      need::<SqlClientService>().flat_map(|client| {
        client
          .begin()
          .map_err(E::from)
          .map(|tx| {
            hlist![
              SqlClientService::new(TransactionSqlClient::new(Arc::clone(&tx))),
              SqlTransactionService(tx)
            ]
          })
          .narrow()
      })
    },
    |env: IsolatedDbEnv, _kind: ExitKind| quietly(env.1.0.0.rollback()),
  )
}

/// A [`SqlClient`] whose every statement runs in one transaction it does not own: the client
/// [`isolated_db()`] provides, usable over any transaction.
///
/// - [`query`](SqlClient::query) / [`execute`](SqlClient::execute) run in the transaction, each
///   under a savepoint of its own, so a failing statement undoes only itself and the
///   transaction stays usable (on PostgreSQL a failed statement would otherwise abort it).
///   Statements of concurrent fibers run one at a time.
/// - [`begin`](SqlClient::begin) / [`begin_with`](SqlClient::begin_with) open a savepoint: the
///   returned transaction's `commit` releases it and its `rollback` rolls back to it. Options
///   are ignored (the outer transaction's isolation level applies).
/// - [`connect`](SqlClient::connect) and [`close`](SqlClient::close) do nothing: whoever began
///   the transaction finishes it.
#[derive(Clone)]
pub struct TransactionSqlClient {
  tx: Arc<dyn SqlTransaction>,
  statements: Semaphore,
}

impl TransactionSqlClient {
  /// A client running everything in `tx`.
  pub fn new(tx: Arc<dyn SqlTransaction>) -> Self {
    Self {
      tx,
      statements: Semaphore::new(1),
    }
  }

  /// The transaction.
  pub fn transaction(&self) -> &Arc<dyn SqlTransaction> {
    &self.tx
  }

  /// `run` on the transaction under a fresh savepoint, one statement at a time.
  fn statement<A: Send + 'static>(
    &self,
    run: impl FnOnce(Arc<dyn SqlTransaction>) -> Effect<A, SqlError> + Send + 'static,
  ) -> Effect<A, SqlError> {
    self
      .statements
      .with_permit(savepoint(&self.tx, "stmt", run))
  }
}

impl fmt::Debug for TransactionSqlClient {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("TransactionSqlClient")
      .field("tx", &"<dyn SqlTransaction>")
      .finish()
  }
}

impl SqlClient for TransactionSqlClient {
  fn connect(&self) -> Effect<(), SqlError> {
    succeed(())
  }

  fn query(&self, sql: &str, params: &[SqlValue]) -> Effect<Vec<SqlRow>, SqlError> {
    let (sql, params) = (sql.to_owned(), params.to_vec());
    self.statement(move |tx| tx.query(&sql, &params))
  }

  fn execute(&self, sql: &str, params: &[SqlValue]) -> Effect<u64, SqlError> {
    let (sql, params) = (sql.to_owned(), params.to_vec());
    self.statement(move |tx| tx.execute(&sql, &params))
  }

  fn begin(&self) -> Effect<Arc<dyn SqlTransaction>, SqlError> {
    let tx = Arc::clone(&self.tx);
    suspend(move || {
      let name = unique_name("id_effect_test_tx");
      tx.create_savepoint(&name).map(move |()| {
        Arc::new(SavepointTransaction {
          tx,
          name,
          finished: Arc::new(AtomicBool::new(false)),
        }) as Arc<dyn SqlTransaction>
      })
    })
  }

  fn begin_with(&self, _options: TransactionOptions) -> Effect<Arc<dyn SqlTransaction>, SqlError> {
    self.begin()
  }
}

/// A "transaction" begun on a [`TransactionSqlClient`]: a savepoint of the outer transaction.
struct SavepointTransaction {
  tx: Arc<dyn SqlTransaction>,
  name: String,
  finished: Arc<AtomicBool>,
}

impl SavepointTransaction {
  fn finished() -> SqlError {
    SqlError::TransactionFailed("transaction already finished".into())
  }

  /// `run` unless the transaction is finished when the effect runs.
  fn open<A: Send + 'static>(&self, run: Effect<A, SqlError>) -> Effect<A, SqlError> {
    let finished = Arc::clone(&self.finished);
    suspend(move || {
      if finished.load(Ordering::Acquire) {
        fail(Self::finished())
      } else {
        run
      }
    })
  }

  /// `run` once, marking the transaction finished when the effect runs (an effect built and
  /// never run, like a bracket's commit after a failure, finishes nothing).
  fn finish(&self, run: Effect<(), SqlError>) -> Effect<(), SqlError> {
    let finished = Arc::clone(&self.finished);
    suspend(move || {
      if finished.swap(true, Ordering::AcqRel) {
        fail(Self::finished())
      } else {
        run
      }
    })
  }
}

impl SqlTransaction for SavepointTransaction {
  fn query(&self, sql: &str, params: &[SqlValue]) -> Effect<Vec<SqlRow>, SqlError> {
    self.open(self.tx.query(sql, params))
  }

  fn execute(&self, sql: &str, params: &[SqlValue]) -> Effect<u64, SqlError> {
    self.open(self.tx.execute(sql, params))
  }

  fn commit(&self) -> Effect<(), SqlError> {
    self.finish(self.tx.release_savepoint(&self.name))
  }

  fn rollback(&self) -> Effect<(), SqlError> {
    let release = self.tx.release_savepoint(&self.name);
    self.finish(self.tx.rollback_to_savepoint(&self.name).then(release))
  }

  fn create_savepoint(&self, name: &str) -> Effect<(), SqlError> {
    self.open(self.tx.create_savepoint(name))
  }

  fn release_savepoint(&self, name: &str) -> Effect<(), SqlError> {
    self.open(self.tx.release_savepoint(name))
  }

  fn rollback_to_savepoint(&self, name: &str) -> Effect<(), SqlError> {
    self.open(self.tx.rollback_to_savepoint(name))
  }
}

// ---------------------------------------------------------------------------------------------
// Clone mode
// ---------------------------------------------------------------------------------------------

/// The database a [`TemplateDb::isolated_db`] layer cloned for one test (provided next to its
/// `SqlClientService`): its name, and its URL for code that opens connections of its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IsolatedDatabase {
  name: String,
  url: String,
  template: String,
  /// The URL of the server's maintenance database (where it is dropped from).
  server: String,
}

impl IsolatedDatabase {
  /// The database's name (`id_effect_test_<pid>_<n>`).
  pub fn name(&self) -> &str {
    &self.name
  }

  /// `postgres://…/<name>`.
  pub fn url(&self) -> String {
    self.url.clone()
  }

  /// The template database it was cloned from.
  pub fn template(&self) -> &str {
    &self.template
  }
}

/// How a [`TemplateDb`] prepares its template: the schema (and seed rows) every test starts
/// from, run once per server on a client connected to the template database.
pub type TemplateSetup = fn(Arc<dyn SqlClient>) -> Effect<(), SqlError>;

/// Clone-mode isolation (PostgreSQL): a template database prepared once per test binary, cloned
/// into a fresh database for each test. See the [module docs](self#clone-mode).
///
/// Keep it in a `static`. The first [`isolated_db`](Self::isolated_db) build on a server creates
/// `id_effect_tpl_<pid>_<n>` there and runs the setup into it (concurrent first builds wait for
/// one setup); a failed setup is remembered and returned to every later build. The template stays
/// on the server (removed with a container).
pub struct TemplateDb {
  setup: TemplateSetup,
  /// Serializes template creation and clones (PostgreSQL refuses to clone a template another
  /// session is connected to).
  lock: OnceLock<Semaphore>,
  /// Template per server (keyed by the server's maintenance URL).
  templates: Mutex<Vec<(String, Result<String, SqlError>)>>,
}

impl TemplateDb {
  /// A template prepared by `setup` (e.g. `|db| migrate(db, "migrations")`).
  pub const fn new(setup: TemplateSetup) -> Self {
    Self {
      setup,
      lock: OnceLock::new(),
      templates: Mutex::new(Vec::new()),
    }
  }

  fn lock(&self) -> Semaphore {
    self.lock.get_or_init(|| Semaphore::new(1)).clone()
  }

  fn cached(&self, server: &str) -> Option<Result<String, SqlError>> {
    let templates = self
      .templates
      .lock()
      .unwrap_or_else(PoisonError::into_inner);
    templates
      .iter()
      .find(|(url, _)| url == server)
      .map(|(_, template)| template.clone())
  }

  fn remember(&self, server: String, template: Result<String, SqlError>) {
    let mut templates = self
      .templates
      .lock()
      .unwrap_or_else(PoisonError::into_inner);
    templates.push((server, template));
  }

  /// The template database on `pg`'s server: created and set up by the first call, the same
  /// name (or setup error) on every later one.
  pub fn template(&'static self, pg: &PostgresContainer) -> Effect<String, SqlError> {
    let (server, pg) = (pg.url(), pg.clone());
    self.lock().with_permit(suspend(move || {
      if let Some(template) = self.cached(&server) {
        return ready(template);
      }
      let name = unique_name("id_effect_tpl");
      let (setup, url) = (self.setup, pg.url_for(&name));
      let (ok, failed, cleanup) = (name.clone(), server.clone(), server.clone());
      let drop_name = name.clone();
      create_database(server.clone(), name, None)
        .flat_map(move |()| on_database(url, setup))
        .map(move |()| {
          self.remember(server, Ok(ok.clone()));
          ok
        })
        .catch(move |e| {
          self.remember(failed, Err(e.clone()));
          widen(drop_database(cleanup, drop_name)).flat_map(move |()| fail(e))
        })
    }))
  }

  /// Clone-mode isolation: a scoped [`Layer`] over the [`PostgresContainer`] it needs (a
  /// [`Shared`](crate::Shared) one, typically) that clones the [template](Self::template) into a
  /// fresh database and provides a client over it ([`ClonedDbEnv`]: `SqlClientService`, a
  /// `PgSqlClient` pool, and [`IsolatedDatabase`]). When its scope closes the pool is closed and
  /// the database dropped, whatever the program's exit. Needs a tokio runtime (sqlx).
  pub fn isolated_db<E>(&'static self) -> Layer<ClonedDbEnv, E, HCons<PostgresContainer, HNil>>
  where
    E: From<SqlError> + Send + 'static,
  {
    Layer::acquire_release(
      move || {
        need::<PostgresContainer>()
          .flat_map(move |pg| self.clone_for(&pg).map_err(E::from).narrow())
      },
      |env: ClonedDbEnv, _kind: ExitKind| {
        let (server, name) = (env.1.0.server.clone(), env.1.0.name.clone());
        env
          .0
          .close()
          .flat_map(move |()| drop_database(server, name))
      },
    )
  }

  /// A fresh clone of the template with a pool over it.
  fn clone_for(&'static self, pg: &PostgresContainer) -> Effect<ClonedDbEnv, SqlError> {
    let (server, pg) = (pg.url(), pg.clone());
    self.template(&pg).flat_map(move |template| {
      let name = unique_name("id_effect_test");
      let db = IsolatedDatabase {
        url: pg.url_for(&name),
        name: name.clone(),
        template: template.clone(),
        server: server.clone(),
      };
      let cleanup = (server.clone(), name.clone());
      self
        .lock()
        .with_permit(create_database(server, name, Some(template)))
        .flat_map(move |()| open_pool(db.url(), TEST_POOL_SIZE).map(move |client| (client, db)))
        .catch(move |e| widen(drop_database(cleanup.0, cleanup.1)).flat_map(move |()| fail(e)))
        .map(|(client, db)| hlist![SqlClientService::new(client), db])
    })
  }
}

impl fmt::Debug for TemplateDb {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let templates = self
      .templates
      .lock()
      .unwrap_or_else(PoisonError::into_inner);
    f.debug_struct("TemplateDb")
      .field("templates", &*templates)
      .finish()
  }
}

/// Connections of the pool over a cloned database.
const TEST_POOL_SIZE: u32 = 8;

/// A `PgSqlClient` over `url` with up to `max_size` connections (opened and checked now).
fn open_pool(url: String, max_size: u32) -> Effect<PgSqlClient, SqlError> {
  from_async(move || async move {
    let config = PgPoolConfig::from_url(url).with_max_size(max_size);
    pg_pool_from_config(config)
      .await
      .map(PgSqlClient::new)
      .map_err(|e| e.0)
  })
}

/// Runs `f` on a short-lived client over `url`, closing it however `f` ends.
fn on_database<A, F>(url: String, f: F) -> Effect<A, SqlError>
where
  A: Send + 'static,
  F: FnOnce(Arc<dyn SqlClient>) -> Effect<A, SqlError> + Send + 'static,
{
  acquire_use_release(
    open_pool(url, 1).map(|client| Arc::new(client) as Arc<dyn SqlClient>),
    f,
    |client, _kind| client.close(),
  )
}

/// `CREATE DATABASE "name"` (from `template`, if any) on the server at `server`.
fn create_database(server: String, name: String, template: Option<String>) -> Effect<(), SqlError> {
  let sql = match template {
    Some(template) => format!("CREATE DATABASE \"{name}\" TEMPLATE \"{template}\""),
    None => format!("CREATE DATABASE \"{name}\""),
  };
  on_database(server, move |db| db.execute(&sql, &[]).map(|_| ()))
}

/// `DROP DATABASE IF EXISTS "name" WITH (FORCE)`, ignoring failures (cleanup).
fn drop_database(server: String, name: String) -> Effect<(), Never> {
  let sql = format!("DROP DATABASE IF EXISTS \"{name}\" WITH (FORCE)");
  quietly(on_database(server, move |db| {
    db.execute(&sql, &[]).map(|_| ())
  }))
}

/// Runs the migrations in directory `dir` ([`Migrator::from_dir`](id_effect_sql::migrate::Migrator::from_dir))
/// on `db`: a [`TemplateSetup`] body, e.g.
/// `TemplateDb::new(|db| migrate(db, concat!(env!("CARGO_MANIFEST_DIR"), "/migrations")))`.
/// A migration failure is returned as its database error, or as [`SqlError::QueryFailed`]
/// naming the directory.
#[cfg(feature = "migrate")]
pub fn migrate(db: Arc<dyn SqlClient>, dir: &str) -> Effect<(), SqlError> {
  use id_effect_sql::migrate::{MigrateError, Migrator};

  let dir = dir.to_owned();
  let to_sql = {
    let dir = dir.clone();
    move |e: MigrateError| match e {
      MigrateError::Failed { error, .. } | MigrateError::Sql(error) => error,
      other => SqlError::QueryFailed {
        sql: format!("migrations in {dir}"),
        message: other.to_string(),
      },
    }
  };
  match Migrator::from_dir(&dir) {
    Ok(migrator) => migrator.run_on(db).map(|_| ()).map_err(to_sql),
    Err(e) => fail(to_sql(e)),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn names_are_unique_identifiers() {
    let (a, b) = (unique_name("id_effect_test"), unique_name("id_effect_test"));
    assert_ne!(a, b);
    assert!(a.starts_with(&format!("id_effect_test_{}_", std::process::id())));
    assert!(a.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
  }

  #[test]
  fn an_isolated_database_reports_where_it_is() {
    let db = IsolatedDatabase {
      name: "t".into(),
      url: "postgres://u:p@localhost:5432/t".into(),
      template: "tpl".into(),
      server: "postgres://u:p@localhost:5432/postgres".into(),
    };
    assert_eq!((db.name(), db.template()), ("t", "tpl"));
    assert_eq!(db.url(), "postgres://u:p@localhost:5432/t");
  }

  #[test]
  fn a_template_starts_empty() {
    static T: TemplateDb = TemplateDb::new(|_| succeed(()));
    assert!(format!("{T:?}").contains("templates: []"));
    assert!(T.cached("postgres://x").is_none());
    let client = Arc::new(id_effect_sql::TestSqlClient::new());
    assert!(matches!(
      (T.setup)(client).block_on(),
      id_effect::Exit::Success(())
    ));
  }
}
