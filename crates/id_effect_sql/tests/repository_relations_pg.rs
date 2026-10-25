//! The relations suite of `tests/relations.rs` on PostgreSQL (features `repository`, `pg`): one
//! container per test binary (`id_effect_test::Shared<Postgres>`, a random host port), a fresh
//! database per test, never the ambient `DATABASE_URL`.
//!
//! Without Docker the tests print `SKIP` and pass, unless `ID_EFFECT_REQUIRE_PG=1` or
//! `ID_EFFECT_REQUIRE_DOCKER=1` (CI), which turn a missing Docker into a failure.

#![cfg(all(feature = "repository", feature = "pg"))]

#[macro_use]
#[allow(dead_code)]
#[path = "relations.rs"]
mod relations;

mod pg {
  use std::sync::Arc;
  use std::sync::atomic::{AtomicUsize, Ordering};

  use id_effect::{Cause, Exit};
  use id_effect_sql::pg::{PgPoolConfig, PgSqlClient, pg_pool_from_config};
  use id_effect_sql::{Dialect, SqlClient};
  use id_effect_test::{ContainerError, Postgres, Shared, docker_available};

  use super::relations::Db;

  static PG: Shared<Postgres> = Shared::new(Postgres::new);

  fn required() -> bool {
    ["ID_EFFECT_REQUIRE_PG", "ID_EFFECT_REQUIRE_DOCKER"]
      .iter()
      .any(|var| std::env::var(var).is_ok_and(|v| v == "1"))
  }

  /// A connected client on a fresh database of the shared container, or `None` (skip).
  async fn live_pg(test: &str) -> Option<Db> {
    if let Err(e) = docker_available() {
      assert!(
        !required(),
        "{test}: ID_EFFECT_REQUIRE_PG=1 but PostgreSQL cannot start: {e}"
      );
      eprintln!("SKIP {test}: {e} (ID_EFFECT_REQUIRE_PG=1 to require it)");
      return None;
    }
    let runtime = id_effect_tokio::TokioRuntime::current().expect("tokio");
    let pg = match runtime.run(PG.get::<ContainerError, _>()).await {
      Exit::Success(pg) => pg,
      Exit::Failure(Cause::Fail(e)) => panic!("{test}: PostgreSQL container: {e}"),
      Exit::Failure(c) => panic!("{test}: PostgreSQL container: {c:?}"),
    };
    // One database per test: tests of one binary share the container under `cargo test`.
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let database = format!(
      "rel_{}_{}",
      std::process::id(),
      NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let admin = connect(&pg.url()).await;
    let create = admin.execute(&format!("CREATE DATABASE {database}"), &[]);
    let created = runtime.run(create).await;
    assert!(
      matches!(created, Exit::Success(_)),
      "{test}: CREATE DATABASE: {created:?}"
    );
    let _ = runtime.run(admin.close()).await;
    Some(Db {
      client: Arc::new(connect(&pg.url_for(&database)).await),
      dialect: Dialect::Postgres,
    })
  }

  async fn connect(url: &str) -> PgSqlClient {
    let config = PgPoolConfig::from_url(url.to_owned()).with_max_size(2);
    let pool = pg_pool_from_config(config)
      .await
      .unwrap_or_else(|e| panic!("pool: {}", e.0));
    let client = PgSqlClient::new(pool);
    let connected = id_effect_tokio::TokioRuntime::current()
      .expect("tokio")
      .run(client.connect())
      .await;
    assert!(matches!(connected, Exit::Success(())), "{connected:?}");
    client
  }

  relations_suite!(live_pg, super::relations);
}
