//! [`TenantRouter`] — one database per tenant (eco.tenancy #8, ADR 0015 D10).
//!
//! A [`TenantRouter`] maps a tenant id to that tenant's own client (a pool, or a
//! [`RoutingSqlClient`](crate::RoutingSqlClient) with read replicas), from a configured map
//! ([`TenantRouter::from_map`], `TenantRouter::from_pg_configs` with feature `pg`) or a resolver
//! effect ([`TenantRouter::from_resolver`]). It is itself a [`SqlClient`], so it goes wherever a
//! client goes; each statement runs on the client of the tenant the fiber acts for
//! ([`TenantRouter::with_tenant`]), or of a fixed tenant through
//! [`TenantRouter::for_tenant`].
//!
//! Secure by default (ADR 0013 R1, R4):
//!
//! - **Fails closed.** A fiber with no tenant, or a tenant the map / resolver does not know, has
//!   no connection: every statement fails with [`SqlError::NotConnected`] and nothing is sent to
//!   any database. There is no default database to fall back to.
//! - **Bounded.** At most [`max_pools`](TenantRouter::max_pools) tenant clients are open
//!   ([`DEFAULT_MAX_TENANT_POOLS`] = 64 unless [`with_max_pools`](TenantRouter::with_max_pools)
//!   says otherwise). Opening one more closes the least recently used one first; its tenant
//!   reconnects on its next statement.
//!
//! The router owns the tenant clients: it connects each on its first statement and closes it on
//! eviction and on [`SqlClient::close`]. Closing a sqlx pool waits for the connections in use to
//! be returned, so an evicted tenant's running transaction finishes first.
//!
//! The tenant id is used as a map key and handed to the resolver as it is: resolve it from an
//! authenticated source (`id_effect_authz::TenantResolver`) and never build a database name or URL
//! from an id that was not validated. Rows of a shared database are still filtered by
//! `CurrentTenant` (feature `repository`); the kit feature
//! `tenancy` sets both from one resolved tenant.
//!
//! ```rust
//! use id_effect::{Cause, Exit};
//! use id_effect_sql::{SqlClient, SqlError, TenantRouter, TestSqlClient, tenant_database};
//!
//! let (acme, globex) = (TestSqlClient::new(), TestSqlClient::new());
//! let router = TenantRouter::from_map([
//!   ("acme", tenant_database(acme.clone())),
//!   ("globex", tenant_database(globex.clone())),
//! ]);
//!
//! let write = TenantRouter::with_tenant("globex", router.execute("INSERT INTO t VALUES (1)", &[]));
//! assert!(matches!(write.block_on(), Exit::Success(0)));
//! assert_eq!((acme.recorded_executes().len(), globex.recorded_executes().len()), (0, 1));
//!
//! // No tenant, or one the router does not know: no connection.
//! let no_tenant = router.query("SELECT 1", &[]).block_on();
//! assert!(matches!(no_tenant, Exit::Failure(Cause::Fail(SqlError::NotConnected))));
//! let unknown = router.for_tenant("initech").execute("DELETE FROM t", &[]).block_on();
//! assert!(matches!(unknown, Exit::Failure(Cause::Fail(SqlError::NotConnected))));
//! ```

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError};

use id_effect::{Effect, FiberRef, Never, Stream, fail, succeed, suspend};

use crate::client::SqlClient;
use crate::error::SqlError;
use crate::row::SqlRow;
use crate::transaction::{SqlTransaction, TransactionOptions};
use crate::value::SqlValue;

/// The default bound on open tenant clients ([`TenantRouter::max_pools`]).
pub const DEFAULT_MAX_TENANT_POOLS: usize = 64;

/// A resolver from a tenant id to a new, unconnected client for it: `Ok(None)` when the tenant
/// has no database (the router then fails closed).
pub type TenantConnector =
  dyn Fn(&str) -> Effect<Option<Arc<dyn SqlClient>>, SqlError> + Send + Sync;

/// A factory of one tenant's client in a [`TenantRouter::from_map`] map.
pub type TenantClientFactory = Box<dyn Fn() -> Effect<Arc<dyn SqlClient>, SqlError> + Send + Sync>;

/// A [`TenantRouter::from_map`] entry serving `client` itself (a clone of it each time the tenant
/// opens). The router closes it on eviction and connects it again on the tenant's next
/// statement, so it suits clients that reconnect after [`SqlClient::close`] (a
/// [`RoutingSqlClient`](crate::RoutingSqlClient) of such clients, test doubles). A sqlx pool does
/// not reopen once closed: build a new one per open with [`tenant_database_from`] or
/// `TenantRouter::from_pg_configs` (feature `pg`).
pub fn tenant_database<C: SqlClient + Clone>(client: C) -> TenantClientFactory {
  tenant_database_from(move || succeed(Arc::new(client.clone()) as Arc<dyn SqlClient>))
}

/// A [`TenantRouter::from_map`] entry building the tenant's (unconnected) client with `factory`
/// each time the tenant opens.
pub fn tenant_database_from(
  factory: impl Fn() -> Effect<Arc<dyn SqlClient>, SqlError> + Send + Sync + 'static,
) -> TenantClientFactory {
  Box::new(factory)
}

/// The tenant a fiber's statements are routed by ([`TenantRouter::with_tenant`]).
static ROUTED_TENANT: LazyLock<FiberRef<Option<Arc<str>>>> = LazyLock::new(|| FiberRef::new(None));

/// A [`SqlClient`] routing each statement to the database of the tenant it runs for (see the
/// [module docs](self)). Clones share the open clients.
#[derive(Clone)]
pub struct TenantRouter {
  connector: Arc<TenantConnector>,
  max_pools: usize,
  pools: Arc<Mutex<Pools>>,
}

/// The open tenant clients, with a use counter for least-recently-used eviction.
#[derive(Default)]
struct Pools {
  open: HashMap<Arc<str>, Pool>,
  tick: u64,
}

struct Pool {
  client: Arc<dyn SqlClient>,
  used: u64,
}

impl Pools {
  /// The open client of `tenant`, marked as just used.
  fn get(&mut self, tenant: &str) -> Option<Arc<dyn SqlClient>> {
    self.tick += 1;
    let tick = self.tick;
    self.open.get_mut(tenant).map(|pool| {
      pool.used = tick;
      Arc::clone(&pool.client)
    })
  }

  /// Opens `client` for `tenant` within `max` clients: the client to use and the clients to close
  /// (the evicted ones, or `client` itself when another fiber opened the tenant first).
  fn insert(
    &mut self,
    tenant: Arc<str>,
    client: Arc<dyn SqlClient>,
    max: usize,
  ) -> (Arc<dyn SqlClient>, Vec<Arc<dyn SqlClient>>) {
    if let Some(open) = self.get(&tenant) {
      return (open, vec![client]);
    }
    let mut stale = Vec::new();
    while self.open.len() >= max {
      let lru = self
        .open
        .iter()
        .min_by_key(|(_, pool)| pool.used)
        .map(|(tenant, _)| Arc::clone(tenant))
        .expect("at least one open client");
      stale.extend(self.open.remove(&lru).map(|pool| pool.client));
    }
    self.open.insert(
      tenant,
      Pool {
        client: Arc::clone(&client),
        used: self.tick,
      },
    );
    (client, stale)
  }
}

impl TenantRouter {
  /// Route through `connector`: called with the tenant id the first time a tenant runs a
  /// statement (and again after its client was evicted), it returns a new, unconnected client
  /// for the tenant, or `None` when the tenant has no database.
  pub fn from_resolver(
    connector: impl Fn(&str) -> Effect<Option<Arc<dyn SqlClient>>, SqlError> + Send + Sync + 'static,
  ) -> Self {
    Self {
      connector: Arc::new(connector),
      max_pools: DEFAULT_MAX_TENANT_POOLS,
      pools: Arc::default(),
    }
  }

  /// Route by a configured map from tenant id to a factory of that tenant's (unconnected)
  /// client. Any tenant outside the map has no connection.
  pub fn from_map<K: Into<String>>(
    databases: impl IntoIterator<Item = (K, TenantClientFactory)>,
  ) -> Self {
    let databases: HashMap<String, TenantClientFactory> = databases
      .into_iter()
      .map(|(tenant, factory)| (tenant.into(), factory))
      .collect();
    Self::from_resolver(move |tenant| match databases.get(tenant) {
      Some(factory) => factory().map(Some),
      None => succeed(None),
    })
  }

  /// Route by a configured map from tenant id to its PostgreSQL pool configuration (feature
  /// `pg`): each tenant's pool opens on its first statement and closes on eviction.
  #[cfg(feature = "pg")]
  pub fn from_pg_configs<K: Into<String>>(
    databases: impl IntoIterator<Item = (K, crate::pg::PgPoolConfig)>,
  ) -> Self {
    use crate::pg::{PgSqlClient, pg_pool_from_config_lazy};
    Self::from_map(databases.into_iter().map(|(tenant, config)| {
      let factory: TenantClientFactory =
        Box::new(move || match pg_pool_from_config_lazy(config.clone()) {
          Ok(pool) => succeed(Arc::new(PgSqlClient::new(pool)) as Arc<dyn SqlClient>),
          Err(e) => fail(e.0),
        });
      (tenant, factory)
    }))
  }

  /// Keep at most `max` tenant clients open (at least one; the default is
  /// [`DEFAULT_MAX_TENANT_POOLS`]). Set it before cloning the router.
  #[must_use]
  pub fn with_max_pools(mut self, max: usize) -> Self {
    self.max_pools = max.max(1);
    self
  }

  /// The most tenant clients open at once.
  #[inline]
  pub fn max_pools(&self) -> usize {
    self.max_pools
  }

  /// The number of tenant clients open now (never more than [`max_pools`](Self::max_pools)).
  pub fn pool_count(&self) -> usize {
    self.lock().open.len()
  }

  /// The tenants with an open client, sorted.
  pub fn open_tenants(&self) -> Vec<String> {
    let mut tenants: Vec<String> = self.lock().open.keys().map(|t| t.to_string()).collect();
    tenants.sort_unstable();
    tenants
  }

  /// Runs `effect` acting for `tenant`: the statements it runs through any [`TenantRouter`] go
  /// to that tenant's database, and so do those of the fibers it forks. The previous tenant is
  /// restored however `effect` ends.
  pub fn with_tenant<A, E, R>(tenant: impl AsRef<str>, effect: Effect<A, E, R>) -> Effect<A, E, R>
  where
    A: Send + 'static,
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    ROUTED_TENANT.locally(Some(Arc::from(tenant.as_ref())), effect)
  }

  /// The tenant the current fiber's statements are routed by ([`with_tenant`](Self::with_tenant)),
  /// if any.
  pub fn routed_tenant<E: Send + 'static, R: Send + Sync + 'static>() -> Effect<Option<String>, E, R>
  {
    ROUTED_TENANT
      .get()
      .map(|tenant: Option<Arc<str>>| tenant.map(|t| t.to_string()))
  }

  /// A [`SqlClient`] bound to `tenant` (for jobs and code that knows its tenant): the router's
  /// client of that tenant, whatever tenant the fiber acts for.
  pub fn for_tenant(&self, tenant: impl AsRef<str>) -> TenantSqlClient {
    TenantSqlClient {
      router: self.clone(),
      tenant: Arc::from(tenant.as_ref()),
    }
  }

  /// The connected client of `tenant`: the open one, else a new one from the map or resolver,
  /// connected and cached (evicting the least recently used client beyond
  /// [`max_pools`](Self::max_pools)). Fails with [`SqlError::NotConnected`] for a tenant without
  /// a database, and with the connect error when its database cannot be reached.
  pub fn client_for(&self, tenant: impl AsRef<str>) -> Effect<Arc<dyn SqlClient>, SqlError> {
    self.client_of(Arc::from(tenant.as_ref()))
  }

  fn client_of(&self, tenant: Arc<str>) -> Effect<Arc<dyn SqlClient>, SqlError> {
    let router = self.clone();
    suspend(move || {
      if let Some(open) = router.lock().get(&tenant) {
        return succeed(open);
      }
      let found = (router.connector)(&tenant);
      found.flat_map(move |found| match found {
        None => fail(SqlError::NotConnected),
        Some(client) => client.connect().flat_map(move |()| {
          let (client, stale) = router.lock().insert(tenant, client, router.max_pools);
          close_all(stale).map(move |()| client)
        }),
      })
    })
  }

  /// The client of the tenant the fiber acts for; [`SqlError::NotConnected`] without one.
  fn current(&self) -> Effect<Arc<dyn SqlClient>, SqlError> {
    let router = self.clone();
    ROUTED_TENANT
      .get()
      .flat_map(move |tenant: Option<Arc<str>>| match tenant {
        Some(tenant) => router.client_of(tenant),
        None => fail(SqlError::NotConnected),
      })
  }

  fn lock(&self) -> MutexGuard<'_, Pools> {
    self.pools.lock().unwrap_or_else(PoisonError::into_inner)
  }
}

impl fmt::Debug for TenantRouter {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("TenantRouter")
      .field("max_pools", &self.max_pools)
      .field("open", &self.pool_count())
      .finish_non_exhaustive()
  }
}

/// Closes every client in `clients`, in order.
fn close_all<E: Send + 'static>(clients: Vec<Arc<dyn SqlClient>>) -> Effect<(), E> {
  clients
    .iter()
    .fold(succeed::<(), Never, _>(()), |done, client| {
      done.then(client.close())
    })
    .map_err(|never: Never| match never {})
}

fn query_on(
  client: Effect<Arc<dyn SqlClient>, SqlError>,
  sql: &str,
  params: &[SqlValue],
) -> Effect<Vec<SqlRow>, SqlError> {
  let (sql, params) = (sql.to_owned(), params.to_vec());
  client.flat_map(move |c| c.query(&sql, &params))
}

fn execute_on(
  client: Effect<Arc<dyn SqlClient>, SqlError>,
  sql: &str,
  params: &[SqlValue],
) -> Effect<u64, SqlError> {
  let (sql, params) = (sql.to_owned(), params.to_vec());
  client.flat_map(move |c| c.execute(&sql, &params))
}

fn stream_on(
  client: Effect<Arc<dyn SqlClient>, SqlError>,
  sql: &str,
  params: &[SqlValue],
) -> Stream<SqlRow, SqlError> {
  let (sql, params) = (sql.to_owned(), params.to_vec());
  Stream::from_effect(client.map(|c| vec![c])).flat_map(move |c| c.query_stream(&sql, &params))
}

impl SqlClient for TenantRouter {
  /// Connects the client of the tenant the fiber acts for, if any (tenant clients otherwise
  /// connect on their first statement).
  fn connect(&self) -> Effect<(), SqlError> {
    let router = self.clone();
    ROUTED_TENANT
      .get()
      .flat_map(move |tenant: Option<Arc<str>>| match tenant {
        Some(tenant) => router.client_of(tenant).map(|_| ()),
        None => succeed(()),
      })
  }

  fn query(&self, sql: &str, params: &[SqlValue]) -> Effect<Vec<SqlRow>, SqlError> {
    query_on(self.current(), sql, params)
  }

  fn execute(&self, sql: &str, params: &[SqlValue]) -> Effect<u64, SqlError> {
    execute_on(self.current(), sql, params)
  }

  fn begin(&self) -> Effect<Arc<dyn SqlTransaction>, SqlError> {
    self.current().flat_map(|c| c.begin())
  }

  fn begin_with(&self, options: TransactionOptions) -> Effect<Arc<dyn SqlTransaction>, SqlError> {
    self.current().flat_map(move |c| c.begin_with(options))
  }

  fn query_stream(&self, sql: &str, params: &[SqlValue]) -> Stream<SqlRow, SqlError> {
    stream_on(self.current(), sql, params)
  }

  /// Closes every open tenant client.
  fn close(&self) -> Effect<(), Never> {
    let router = self.clone();
    suspend(move || {
      let open: Vec<Arc<dyn SqlClient>> = router
        .lock()
        .open
        .drain()
        .map(|(_, pool)| pool.client)
        .collect();
      close_all(open)
    })
  }
}

/// A [`SqlClient`] bound to one tenant of a [`TenantRouter`] ([`TenantRouter::for_tenant`]).
///
/// The router owns the tenant's client: [`connect`](SqlClient::connect) opens it, and
/// [`close`](SqlClient::close) does nothing (close the router).
#[derive(Clone)]
pub struct TenantSqlClient {
  router: TenantRouter,
  tenant: Arc<str>,
}

impl TenantSqlClient {
  /// The tenant.
  #[inline]
  pub fn tenant(&self) -> &str {
    &self.tenant
  }

  fn client(&self) -> Effect<Arc<dyn SqlClient>, SqlError> {
    self.router.client_of(Arc::clone(&self.tenant))
  }
}

impl fmt::Debug for TenantSqlClient {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("TenantSqlClient")
      .field("tenant", &self.tenant)
      .finish_non_exhaustive()
  }
}

impl SqlClient for TenantSqlClient {
  fn connect(&self) -> Effect<(), SqlError> {
    self.client().map(|_| ())
  }

  fn query(&self, sql: &str, params: &[SqlValue]) -> Effect<Vec<SqlRow>, SqlError> {
    query_on(self.client(), sql, params)
  }

  fn execute(&self, sql: &str, params: &[SqlValue]) -> Effect<u64, SqlError> {
    execute_on(self.client(), sql, params)
  }

  fn begin(&self) -> Effect<Arc<dyn SqlTransaction>, SqlError> {
    self.client().flat_map(|c| c.begin())
  }

  fn begin_with(&self, options: TransactionOptions) -> Effect<Arc<dyn SqlTransaction>, SqlError> {
    self.client().flat_map(move |c| c.begin_with(options))
  }

  fn query_stream(&self, sql: &str, params: &[SqlValue]) -> Stream<SqlRow, SqlError> {
    stream_on(self.client(), sql, params)
  }
}
