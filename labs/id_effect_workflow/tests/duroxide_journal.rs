//! duroxide step journal integration tests.
//!
//! They need a database at `DATABASE_URL`. With `ID_EFFECT_REQUIRE_PG=1` a missing or
//! unreachable database is a test **failure**; without it the tests print `SKIP` and return.
//!
//! The `StepJournal` impl is a synchronous edge: it is driven from the plain test thread, while
//! the pool is set up on a tokio runtime that stays alive for the test.

use id_effect_sql::pg::{PgPoolConfig, pg_pool_from_config};
use id_effect_workflow::{
  DuroxideStepJournal, StepJournal, WorkflowError, bootstrap_duroxide_schema,
};

fn pg_required() -> bool {
  std::env::var("ID_EFFECT_REQUIRE_PG").is_ok_and(|v| v == "1")
}

fn unavailable<T>(test: &str, why: &str) -> Option<T> {
  assert!(
    !pg_required(),
    "{test}: ID_EFFECT_REQUIRE_PG=1 but Postgres is unavailable: {why}"
  );
  eprintln!("SKIP {test}: {why}");
  None
}

/// A tokio runtime plus a bootstrapped pool on it, or `None` (skip) without a database.
fn live_pool(test: &str) -> Option<(tokio::runtime::Runtime, sqlx::PgPool)> {
  let Ok(url) = std::env::var("DATABASE_URL") else {
    return unavailable(test, "DATABASE_URL unset");
  };
  let rt = tokio::runtime::Builder::new_multi_thread()
    .enable_all()
    .build()
    .expect("tokio runtime");
  let pool = rt.block_on(async {
    tokio::time::timeout(
      std::time::Duration::from_secs(5),
      pg_pool_from_config(PgPoolConfig::from_url(url).with_max_size(2)),
    )
    .await
  });
  let pool = match pool {
    Ok(Ok(pool)) => pool,
    Ok(Err(e)) => return unavailable(test, &e.0.to_string()),
    Err(_) => return unavailable(test, "connect timed out"),
  };
  rt.block_on(bootstrap_duroxide_schema(&pool))
    .expect("bootstrap duroxide schema");
  Some((rt, pool))
}

#[test]
fn live_duroxide_step_journal_resume() {
  let Some((_rt, pool)) = live_pool("live_duroxide_step_journal_resume") else {
    return;
  };

  let wf = format!("wf-{}", uuid::Uuid::new_v4());
  let mut journal = DuroxideStepJournal::new(pool);
  journal.register_workflow(&wf).expect("register");
  assert!(journal.has_workflow(&wf).expect("has"));
  let v: i32 = journal
    .run_step_typed(&wf, 0, "step", || Ok(42))
    .expect("run");
  assert_eq!(v, 42);
  let v2: i32 = journal
    .run_step_typed(&wf, 0, "step", || Ok(99))
    .expect("resume");
  assert_eq!(v2, 42);
  assert_eq!(journal.completed_step_count(&wf).expect("count"), 1);
  assert_eq!(
    journal.completed_json(&wf, 0).expect("json").as_deref(),
    Some("42")
  );
}

#[test]
fn live_duroxide_failed_step_rolls_back_and_duplicate_register_errors() {
  let Some((_rt, pool)) =
    live_pool("live_duroxide_failed_step_rolls_back_and_duplicate_register_errors")
  else {
    return;
  };

  let wf = format!("wf-{}", uuid::Uuid::new_v4());
  let mut journal = DuroxideStepJournal::new(pool);
  journal.migrate_sync().expect("migrate_sync");
  journal.register_workflow(&wf).expect("register");
  assert!(matches!(
    journal.register_workflow(&wf),
    Err(WorkflowError::WorkflowAlreadyExists(_))
  ));
  let err = journal
    .run_step_typed::<i32, _>(&wf, 0, "step", || Err(WorkflowError::InvalidWorkflowId))
    .unwrap_err();
  assert!(matches!(err, WorkflowError::InvalidWorkflowId));
  assert_eq!(journal.completed_step_count(&wf).expect("count"), 0);
  let v: i32 = journal
    .run_step_typed(&wf, 0, "step", || Ok(7))
    .expect("retry after rollback");
  assert_eq!(v, 7);
}

#[tokio::test(flavor = "multi_thread")]
async fn step_journal_edge_is_refused_inside_async_context() {
  let pool = sqlx::postgres::PgPoolOptions::new()
    .connect_lazy("postgres://127.0.0.1:1/none")
    .expect("lazy pool");
  let journal = DuroxideStepJournal::new(pool);
  let caught =
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| journal.has_workflow("wf")));
  assert!(
    caught.is_err(),
    "a blocking edge inside a tokio task must be refused"
  );
}
