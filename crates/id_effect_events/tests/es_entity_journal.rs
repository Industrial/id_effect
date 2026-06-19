//! es-entity journal integration tests (skip when DATABASE_URL unreachable).

use id_effect::run_async;
use id_effect_events::{
  EsEntityEventStore, EsEntityPgBackend, EventStore, apply_es_entity_journal_ddl,
};
use id_effect_sql_pg::{PgPoolConfig, pg_pool_from_config};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
struct Ping {
  n: u32,
}

async fn live_pool() -> Option<sqlx::PgPool> {
  let url = std::env::var("DATABASE_URL").ok()?;
  let pool = tokio::time::timeout(
    std::time::Duration::from_secs(2),
    pg_pool_from_config(PgPoolConfig::from_url(url).with_max_size(2)),
  )
  .await
  .ok()
  .and_then(|r| r.ok())?;
  tokio::time::timeout(
    std::time::Duration::from_secs(2),
    sqlx::query("SELECT 1").execute(&pool),
  )
  .await
  .ok()
  .and_then(|r| r.ok())?;
  Some(pool)
}

#[tokio::test(flavor = "multi_thread")]
async fn live_es_entity_event_store_round_trip() {
  let Some(pool) = live_pool().await else {
    return;
  };
  if apply_es_entity_journal_ddl(&pool).await.is_err() {
    return;
  }

  let stream = format!("es-entity-{}", uuid::Uuid::new_v4());
  let store = EsEntityEventStore::<Ping>::new(EsEntityPgBackend::new(pool));
  let appended = run_async(store.append(&stream, &[Ping { n: 1 }, Ping { n: 2 }]), ())
    .await
    .expect("append");
  assert_eq!(appended.len(), 2);
  let read = run_async(store.read(&stream, 1), ()).await.expect("read");
  assert_eq!(read.len(), 2);
  assert_eq!(read[0].payload, Ping { n: 1 });
}
