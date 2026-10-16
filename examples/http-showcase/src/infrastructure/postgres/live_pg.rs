//! [`LivePg`]: test support for the live PostgreSQL tests of this adapter.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use id_effect::{Effect, Exit};
use id_effect_axum::EffectExecutor;
use id_effect_sql::SqlClientService;
use id_effect_sql::pg::{PgPoolConfig, PgSqlClient, pg_pool_from_config};

use super::Migration;
use crate::domain::todo::Title;

/// A migrated connection to the database at `DATABASE_URL`.
///
/// The live tests need `DATABASE_URL`. With `ID_EFFECT_REQUIRE_PG=1` (CI) a missing or
/// unreachable database **fails** them; otherwise they print `SKIP` and return.
pub(crate) struct LivePg {
  /// The connected, migrated client.
  pub(crate) sql: SqlClientService,
}

impl LivePg {
  /// Set to `1` to make an unavailable database a test failure instead of a skip.
  pub(crate) const REQUIRE_VAR: &'static str = "ID_EFFECT_REQUIRE_PG";

  /// Whether [`Self::REQUIRE_VAR`] is `1`.
  fn required() -> bool {
    std::env::var(Self::REQUIRE_VAR).is_ok_and(|v| v == "1")
  }

  /// Skips `test` (`None`), or fails it when the database is required.
  fn unavailable<T>(test: &str, why: &str) -> Option<T> {
    assert!(
      !Self::required(),
      "{test}: {}=1 but Postgres is unavailable: {why}",
      Self::REQUIRE_VAR
    );
    eprintln!(
      "SKIP {test}: {why} (set DATABASE_URL, and {}=1 to require it)",
      Self::REQUIRE_VAR
    );
    None
  }

  /// `DATABASE_URL`, or `None` after [`Self::unavailable`].
  pub(crate) fn url(test: &str) -> Option<String> {
    match std::env::var("DATABASE_URL") {
      Ok(url) if !url.trim().is_empty() => Some(url),
      _ => Self::unavailable(test, "DATABASE_URL is not set"),
    }
  }

  /// A connected client on `DATABASE_URL` with [`Migration::apply`] run, or `None` after
  /// [`Self::unavailable`].
  pub(crate) async fn connect(test: &str) -> Option<Self> {
    let url = Self::url(test)?;
    let pool = match pg_pool_from_config(PgPoolConfig::from_url(url).with_max_size(4)).await {
      Ok(pool) => pool,
      Err(e) => return Self::unavailable(test, &format!("cannot build pool: {}", e.0)),
    };
    let sql = SqlClientService::new(PgSqlClient::new(pool));
    if let Exit::Failure(cause) = Self::exit(sql.connect()).await {
      return Self::unavailable(test, &format!("cannot connect: {cause:?}"));
    }
    let migrated = Self::exit(Migration::apply(Arc::clone(&sql.0))).await;
    assert!(matches!(migrated, Exit::Success(())), "{migrated:?}");
    Some(Self { sql })
  }

  /// Runs `eff` on the global executor (the pg driver needs tokio).
  pub(crate) async fn exit<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> Exit<A, E> {
    EffectExecutor::global().run_effect(eff).await
  }

  /// A title no other test (or earlier run) uses.
  pub(crate) fn unique_title(label: &str) -> Title {
    static N: AtomicU32 = AtomicU32::new(0);
    let nanos = SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .map_or(0, |d| d.as_nanos());
    let n = N.fetch_add(1, Ordering::SeqCst);
    Title::parse(&format!("{label}-{}-{nanos}-{n}", std::process::id())).unwrap()
  }

  /// Closes the pool.
  pub(crate) async fn close(self) {
    let _ = Self::exit(self.sql.close()).await;
  }
}
