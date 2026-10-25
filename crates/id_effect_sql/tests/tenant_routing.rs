//! `TenantRouter` (ADR 0015 D10, eco.tenancy #8): one database per tenant, failing closed for an
//! unknown or missing tenant, with a bounded number of open tenant clients.
//!
//! The PostgreSQL test (feature `pg`) starts its own container (`id_effect_test::Shared<Postgres>`,
//! a random host port) with two databases, never the ambient `DATABASE_URL`. Without Docker it
//! prints `SKIP` and passes, unless `ID_EFFECT_REQUIRE_PG=1` or `ID_EFFECT_REQUIRE_DOCKER=1`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use id_effect::{Cause, Effect, Exit, Never, succeed};
use id_effect_sql::{
  DEFAULT_MAX_TENANT_POOLS, Isolation, RoutingSqlClient, SqlClient, SqlError, SqlRow, TenantRouter,
  TestSqlClient, tenant_database, with_transaction,
};

fn run<A: Send + 'static>(effect: Effect<A, SqlError>) -> Result<A, SqlError> {
  match effect.block_on() {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(cause) => panic!("unexpected cause: {cause:?}"),
  }
}

fn close(client: &dyn SqlClient) {
  assert_eq!(client.close().block_on(), Exit::<(), Never>::Success(()));
}

/// A router over `tenants`, each with its own test client.
fn router_of(tenants: &[&str]) -> (TenantRouter, Vec<TestSqlClient>) {
  let clients: Vec<TestSqlClient> = tenants.iter().map(|_| TestSqlClient::new()).collect();
  let router = TenantRouter::from_map(
    tenants
      .iter()
      .zip(&clients)
      .map(|(tenant, client)| (*tenant, tenant_database(client.clone()))),
  );
  (router, clients)
}

/// The clients a [`counting_router`] opened, with their tenants, in order.
type Opened = Arc<Mutex<Vec<(String, TestSqlClient)>>>;

/// A router opening a fresh test client per tenant open, and the clients it opened.
fn counting_router() -> (TenantRouter, Opened) {
  let opened = Arc::new(Mutex::new(Vec::new()));
  let log = Arc::clone(&opened);
  let router = TenantRouter::from_resolver(move |tenant| {
    let client = TestSqlClient::new();
    log
      .lock()
      .unwrap()
      .push((tenant.to_owned(), client.clone()));
    succeed(Some(Arc::new(client) as Arc<dyn SqlClient>))
  });
  (router, opened)
}

#[test]
fn unknown_tenant_has_no_connection() {
  let (router, clients) = router_of(&["acme"]);
  let acme = &clients[0];
  // No tenant at all: no statement reaches any database.
  assert_eq!(
    run(router.query("SELECT 1", &[])),
    Err(SqlError::NotConnected)
  );
  assert_eq!(
    run(router.execute("DELETE FROM t", &[])),
    Err(SqlError::NotConnected)
  );
  assert!(matches!(run(router.begin()), Err(SqlError::NotConnected)));
  let stream = router.query_stream("SELECT 1", &[]).run_collect();
  assert_eq!(run(stream), Err(SqlError::NotConnected));
  // A tenant outside the map: the same, through the fiber's tenant or a bound client.
  let unknown = TenantRouter::with_tenant("initech", router.execute("DELETE FROM t", &[]));
  assert_eq!(run(unknown), Err(SqlError::NotConnected));
  let bound = router.for_tenant("initech");
  assert_eq!(
    run(bound.query("SELECT 1", &[])),
    Err(SqlError::NotConnected)
  );
  assert_eq!(run(bound.connect()), Err(SqlError::NotConnected));
  assert!(matches!(
    run(bound.begin_with(Isolation::Serializable.into())),
    Err(SqlError::NotConnected)
  ));
  // A resolver that knows no database for the tenant fails closed too.
  let none = TenantRouter::from_resolver(|_| succeed(None));
  assert_eq!(
    run(none.client_for("acme").map(|_| ())),
    Err(SqlError::NotConnected)
  );
  assert!(acme.recorded_queries().is_empty() && acme.recorded_executes().is_empty());
  assert!(!acme.is_connected());
  assert_eq!((router.pool_count(), none.pool_count()), (0, 0));
  // A tenant whose database cannot be reached is not cached.
  let down = TestSqlClient::new();
  down.fail_on_connect();
  let router = TenantRouter::from_map([("acme", tenant_database(down))]);
  assert_eq!(
    run(router.for_tenant("acme").execute("SELECT 1", &[])),
    Err(SqlError::NotConnected)
  );
  assert_eq!(router.pool_count(), 0);
}

#[test]
fn routes_each_tenant_to_its_own_client() {
  let (router, clients) = router_of(&["acme", "globex"]);
  let (acme, globex) = (&clients[0], &clients[1]);
  acme.script_query("SELECT n FROM t", vec![SqlRow::from_pairs([("n", 1i64)])]);
  globex.script_query("SELECT n FROM t", vec![SqlRow::from_pairs([("n", 2i64)])]);
  let read = |tenant: &str| {
    let rows = run(TenantRouter::with_tenant(
      tenant,
      router.query("SELECT n FROM t", &[]),
    ))
    .expect("read");
    rows[0].get::<i64>("n").expect("n")
  };
  assert_eq!((read("acme"), read("globex")), (1, 2));
  let streamed = TenantRouter::with_tenant(
    "globex",
    router.query_stream("SELECT n FROM t", &[]).run_collect(),
  );
  assert_eq!(run(streamed).expect("stream").len(), 1);
  // The fiber's tenant is scoped to `with_tenant` and visible inside it.
  let inside = TenantRouter::with_tenant("acme", TenantRouter::routed_tenant());
  assert_eq!(run(inside), Ok(Some("acme".to_owned())));
  assert_eq!(run(TenantRouter::routed_tenant()), Ok(None));
  // Transactions run on the tenant's client.
  let tx = TenantRouter::with_tenant(
    "globex",
    with_transaction(Arc::new(router.clone()) as Arc<dyn SqlClient>, |tx| {
      tx.execute("UPDATE t SET n = 3", &[])
    }),
  );
  run(tx).expect("transaction");
  let options = TenantRouter::with_tenant(
    "acme",
    router
      .begin_with(Isolation::Serializable.into())
      .flat_map(|tx| tx.commit()),
  );
  run(options).expect("begin_with");
  assert_eq!((acme.commits(), globex.commits()), (1, 1));
  assert_eq!(router.open_tenants(), ["acme", "globex"]);
  // `connect` opens the fiber's tenant only; without one it opens nothing.
  let (fresh, fresh_clients) = router_of(&["acme"]);
  run(fresh.connect()).expect("connect without a tenant");
  assert_eq!(fresh.pool_count(), 0);
  run(TenantRouter::with_tenant("acme", fresh.connect())).expect("connect acme");
  assert!(fresh_clients[0].is_connected());
  // Closing the router closes every tenant client.
  close(&router);
  assert_eq!(
    (acme.closes(), globex.closes(), router.pool_count()),
    (1, 1, 0)
  );
  assert!(format!("{router:?}").contains("max_pools: 64"));
}

#[test]
fn bound_tenant_client_ignores_the_fiber_tenant() {
  let (router, clients) = router_of(&["acme", "globex"]);
  let acme = router.for_tenant("acme");
  assert_eq!(acme.tenant(), "acme");
  assert!(format!("{acme:?}").contains("\"acme\""));
  run(acme.connect()).expect("connect");
  assert!(clients[0].is_connected() && !clients[1].is_connected());
  let write = TenantRouter::with_tenant("globex", acme.execute("INSERT INTO t VALUES (1)", &[]));
  run(write).expect("write");
  run(acme.query("SELECT 1", &[])).expect("read");
  run(acme.query_stream("SELECT 1", &[]).run_collect()).expect("stream");
  run(acme.begin().flat_map(|tx| tx.commit())).expect("begin");
  run(
    acme
      .begin_with(Isolation::RepeatableRead.into())
      .flat_map(|tx| tx.rollback()),
  )
  .expect("begin_with");
  assert_eq!(clients[0].recorded_executes().len(), 1);
  assert!(clients[1].recorded_executes().is_empty());
  assert_eq!((clients[0].commits(), clients[0].rollbacks()), (1, 1));
  // The router owns the client: closing the bound client leaves it open.
  close(&acme);
  assert_eq!((clients[0].closes(), router.pool_count()), (0, 1));
}

#[test]
fn r4_tenant_pool_count_bounded() {
  let (router, opened) = counting_router();
  assert_eq!(router.max_pools(), DEFAULT_MAX_TENANT_POOLS);
  assert_eq!(DEFAULT_MAX_TENANT_POOLS, 64);
  for n in 0..100 {
    let tenant = format!("t{n}");
    run(router.for_tenant(&tenant).execute("SELECT 1", &[])).expect("statement");
    assert!(router.pool_count() <= 64);
  }
  assert_eq!(router.pool_count(), 64);
  let opened_clients: Vec<(String, TestSqlClient)> = opened.lock().unwrap().clone();
  assert_eq!(opened_clients.len(), 100);
  // The 36 least recently used tenants were closed, the newest 64 are open.
  let closed: usize = opened_clients.iter().map(|(_, c)| c.closes()).sum();
  assert_eq!(closed, 36);
  assert!(opened_clients[..36].iter().all(|(_, c)| c.closes() == 1));
  assert!(opened_clients[36..].iter().all(|(_, c)| c.is_connected()));

  // Least recently *used*, not least recently opened.
  let (router, opened) = counting_router();
  let router = router.with_max_pools(2);
  let touch = |tenant: &str| run(router.client_for(tenant).map(|_| ())).expect("open");
  touch("a");
  touch("b");
  touch("a");
  touch("c");
  assert_eq!(router.open_tenants(), ["a", "c"]);
  let closes: Vec<(String, usize)> = opened
    .lock()
    .unwrap()
    .iter()
    .map(|(t, c)| (t.clone(), c.closes()))
    .collect();
  assert_eq!(closes, [("a".into(), 0), ("b".into(), 1), ("c".into(), 0)]);
  // An evicted tenant reopens on its next statement, with a new client.
  touch("b");
  assert_eq!(router.open_tenants(), ["b", "c"]);
  assert_eq!(opened.lock().unwrap().len(), 4);
  // The bound is at least one client.
  assert_eq!(router.with_max_pools(0).max_pools(), 1);
}

#[test]
fn concurrent_open_keeps_the_first_client() {
  // The resolver opens the same tenant again before returning its own client, as a second fiber
  // racing the first would: the router keeps the client already open and closes the other.
  let router_cell: Arc<OnceLock<TenantRouter>> = Arc::new(OnceLock::new());
  let (first, second) = (TestSqlClient::new(), TestSqlClient::new());
  let calls = Arc::new(AtomicUsize::new(0));
  let (cell, a, b, n) = (
    Arc::clone(&router_cell),
    first.clone(),
    second.clone(),
    Arc::clone(&calls),
  );
  let router = TenantRouter::from_resolver(move |tenant| {
    if n.fetch_add(1, Ordering::SeqCst) == 0 {
      let raced = Arc::new(a.clone()) as Arc<dyn SqlClient>;
      cell
        .get()
        .expect("router")
        .client_for(tenant)
        .map(move |_| Some(raced))
    } else {
      succeed(Some(Arc::new(b.clone()) as Arc<dyn SqlClient>))
    }
  });
  router_cell.set(router.clone()).expect("set once");
  run(
    router
      .for_tenant("acme")
      .execute("INSERT INTO t VALUES (1)", &[]),
  )
  .expect("write");
  assert_eq!(calls.load(Ordering::SeqCst), 2);
  assert_eq!((first.closes(), second.closes()), (1, 0));
  assert_eq!(second.recorded_executes().len(), 1);
  assert_eq!(router.pool_count(), 1);
}

#[test]
fn tenant_router_composes_with_replicas() {
  // Each tenant's database is a primary with a read replica.
  let tenants: Vec<(TestSqlClient, TestSqlClient)> = (0..2)
    .map(|_| (TestSqlClient::new(), TestSqlClient::new()))
    .collect();
  let router = TenantRouter::from_map(tenants.iter().zip(["acme", "globex"]).map(
    |((primary, replica), tenant)| {
      let routed = RoutingSqlClient::new(
        Arc::new(primary.clone()),
        vec![Arc::new(replica.clone()) as Arc<dyn SqlClient>],
      );
      (tenant, tenant_database(routed))
    },
  ));
  let (acme_primary, acme_replica) = &tenants[0];
  let (globex_primary, globex_replica) = &tenants[1];
  let work = |tenant: &str| {
    TenantRouter::with_tenant(
      tenant,
      router
        .query("SELECT * FROM docs", &[])
        .then(router.execute("INSERT INTO docs VALUES (1)", &[]))
        .then(with_transaction(
          Arc::new(router.clone()) as Arc<dyn SqlClient>,
          |tx| tx.query("SELECT * FROM docs FOR UPDATE", &[]),
        )),
    )
  };
  run(work("acme")).expect("acme");
  // Reads went to acme's replica, the write and the transaction to acme's primary.
  assert_eq!(acme_replica.recorded_queries().len(), 1);
  assert_eq!(acme_primary.recorded_executes().len(), 1);
  assert_eq!(acme_primary.recorded_queries().len(), 1);
  assert_eq!(acme_primary.commits(), 1);
  // Nothing reached globex.
  for client in [globex_primary, globex_replica] {
    assert!(client.recorded_queries().is_empty() && client.recorded_executes().is_empty());
    assert!(!client.is_connected());
  }
  run(work("globex")).expect("globex");
  assert_eq!(globex_replica.recorded_queries().len(), 1);
  assert_eq!(globex_primary.recorded_executes().len(), 1);
  // Closing the router closes each tenant's primary and replica.
  close(&router);
  assert!(
    tenants
      .iter()
      .all(|(p, r)| (p.closes(), r.closes()) == (1, 1))
  );
}

#[cfg(feature = "pg")]
mod pg {
  use std::sync::atomic::{AtomicUsize, Ordering};

  use id_effect::{Cause, Exit};
  use id_effect_sql::pg::{PgPoolConfig, PgSqlClient, pg_pool_from_config};
  use id_effect_sql::{SqlClient, SqlError, TenantRouter};
  use id_effect_test::{ContainerError, Postgres, Shared, docker_available};

  static PG: Shared<Postgres> = Shared::new(Postgres::new);

  fn required() -> bool {
    ["ID_EFFECT_REQUIRE_PG", "ID_EFFECT_REQUIRE_DOCKER"]
      .iter()
      .any(|var| std::env::var(var).is_ok_and(|v| v == "1"))
  }

  async fn run<A: Send + 'static>(effect: id_effect::Effect<A, SqlError>) -> Result<A, SqlError> {
    let runtime = id_effect_tokio::TokioRuntime::current().expect("tokio");
    match runtime.run(effect).await {
      Exit::Success(a) => Ok(a),
      Exit::Failure(Cause::Fail(e)) => Err(e),
      Exit::Failure(cause) => panic!("unexpected cause: {cause:?}"),
    }
  }

  #[tokio::test]
  async fn invalid_tenant_config_has_no_connection() {
    let router = TenantRouter::from_pg_configs([("acme", PgPoolConfig::from_url("not a url"))]);
    assert!(
      run(router.for_tenant("acme").query("SELECT 1", &[]))
        .await
        .is_err()
    );
    assert_eq!(router.pool_count(), 0);
  }

  #[tokio::test(flavor = "multi_thread")]
  async fn tenant_router_routes_two_tenants_to_two_databases() {
    const TEST: &str = "tenant_router_routes_two_tenants_to_two_databases";
    if let Err(e) = docker_available() {
      assert!(
        !required(),
        "{TEST}: ID_EFFECT_REQUIRE_PG=1 but PostgreSQL cannot start: {e}"
      );
      eprintln!("SKIP {TEST}: {e} (ID_EFFECT_REQUIRE_PG=1 to require it)");
      return;
    }
    let runtime = id_effect_tokio::TokioRuntime::current().expect("tokio");
    let pg = match runtime.run(PG.get::<ContainerError, _>()).await {
      Exit::Success(pg) => pg,
      Exit::Failure(Cause::Fail(e)) => panic!("{TEST}: PostgreSQL container: {e}"),
      Exit::Failure(c) => panic!("{TEST}: PostgreSQL container: {c:?}"),
    };
    // One container, two databases (unique per run: tests of a binary share the container).
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let suffix = format!(
      "{}_{}",
      std::process::id(),
      NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let (acme_db, globex_db) = (format!("acme_{suffix}"), format!("globex_{suffix}"));
    let pool = pg_pool_from_config(PgPoolConfig::from_url(pg.url()).with_max_size(1))
      .await
      .unwrap_or_else(|e| panic!("{TEST}: admin pool: {}", e.0));
    let admin = PgSqlClient::new(pool);
    for db in [&acme_db, &globex_db] {
      run(admin.execute(&format!("CREATE DATABASE {db}"), &[]))
        .await
        .unwrap_or_else(|e| panic!("{TEST}: CREATE DATABASE {db}: {e:?}"));
    }
    let _ = runtime.run(admin.close()).await;

    let router = TenantRouter::from_pg_configs([
      (
        "acme",
        PgPoolConfig::from_url(pg.url_for(&acme_db)).with_max_size(2),
      ),
      (
        "globex",
        PgPoolConfig::from_url(pg.url_for(&globex_db)).with_max_size(2),
      ),
    ]);
    for tenant in ["acme", "globex"] {
      let setup = TenantRouter::with_tenant(
        tenant,
        router
          .execute("CREATE TABLE docs (title TEXT NOT NULL)", &[])
          .then(router.execute(
            "INSERT INTO docs (title) VALUES ($1)",
            &[format!("{tenant} report").into()],
          )),
      );
      run(setup).await.expect("setup");
    }
    // Each tenant sees its own database only, and which database it is.
    for (tenant, db) in [("acme", &acme_db), ("globex", &globex_db)] {
      let rows = run(TenantRouter::with_tenant(
        tenant,
        router.query("SELECT title, current_database() AS db FROM docs", &[]),
      ))
      .await
      .expect("read");
      assert_eq!(rows.len(), 1, "{tenant}");
      assert_eq!(
        rows[0].get::<String>("title"),
        Ok(format!("{tenant} report"))
      );
      assert_eq!(rows[0].get::<String>("db"), Ok(db.clone()));
    }
    assert_eq!(router.open_tenants(), ["acme", "globex"]);
    // An unknown tenant reaches neither database.
    let unknown = TenantRouter::with_tenant("initech", router.query("SELECT 1", &[]));
    assert_eq!(run(unknown).await, Err(SqlError::NotConnected));
    let _ = runtime.run(router.close()).await;
    assert_eq!(router.pool_count(), 0);
  }
}
