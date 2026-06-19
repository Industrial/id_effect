//! duroxide step journal integration tests (skip when DATABASE_URL unreachable).

use id_effect_sql_pg::{PgPoolConfig, pg_pool_from_config};
use id_effect_workflow::{DuroxideStepJournal, StepJournal, bootstrap_duroxide_schema};

async fn live_pool() -> Option<sqlx::PgPool> {
  let url = std::env::var("DATABASE_URL").ok()?;
  tokio::time::timeout(
    std::time::Duration::from_secs(5),
    pg_pool_from_config(PgPoolConfig::from_url(url).with_max_size(2)),
  )
  .await
  .ok()
  .and_then(|r| r.ok())
}

#[tokio::test(flavor = "multi_thread")]
async fn live_duroxide_step_journal_resume() {
  let Some(pool) = live_pool().await else {
    return;
  };
  if bootstrap_duroxide_schema(&pool).await.is_err() {
    return;
  }

  let wf = format!("wf-{}", uuid::Uuid::new_v4());
  let mut journal = DuroxideStepJournal::new(pool);
  journal.register_workflow(&wf).expect("register");
  let v: i32 = journal
    .run_step_typed(&wf, 0, "step", || Ok(42))
    .expect("run");
  assert_eq!(v, 42);
  let v2: i32 = journal
    .run_step_typed(&wf, 0, "step", || Ok(99))
    .expect("resume");
  assert_eq!(v2, 42);
}
