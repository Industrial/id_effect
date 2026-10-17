//! Chaos (test.chaos #2 #7): a seeded fault plan slows the posting path's database down and
//! makes it lose serialization conflicts; the posting's retry policy still posts every entry,
//! once, and the same seed gives the same schedule.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{As, with_ledger};
use id_effect::{Effect, Never, Stream};
use id_effect_kit::sql::{
  SqlClient, SqlError, SqlRow, SqlTransaction, SqlValue, TransactionOptions,
};
use id_effect_kit::test::http::StatusCode;
use id_effect_kit::test::{Fault, FaultKind, FaultPlan};
use serde_json::Value;

/// A client whose `SERIALIZABLE` transactions (the postings') start under `plan`: late, or
/// failing as a serialization conflict (SQLSTATE 40001), as the plan decides.
struct Faulty {
  inner: Arc<dyn SqlClient>,
  plan: FaultPlan<SqlError>,
}

impl SqlClient for Faulty {
  fn connect(&self) -> Effect<(), SqlError> {
    self.inner.connect()
  }

  fn query(&self, sql: &str, params: &[SqlValue]) -> Effect<Vec<SqlRow>, SqlError> {
    self.inner.query(sql, params)
  }

  fn execute(&self, sql: &str, params: &[SqlValue]) -> Effect<u64, SqlError> {
    self.inner.execute(sql, params)
  }

  fn begin(&self) -> Effect<Arc<dyn SqlTransaction>, SqlError> {
    self.inner.begin()
  }

  fn begin_with(&self, options: TransactionOptions) -> Effect<Arc<dyn SqlTransaction>, SqlError> {
    self.plan.inject(self.inner.begin_with(options))
  }

  fn query_stream(&self, sql: &str, params: &[SqlValue]) -> Stream<SqlRow, SqlError> {
    self.inner.query_stream(sql, params)
  }

  fn close(&self) -> Effect<(), Never> {
    self.inner.close()
  }
}

/// Seed 7: about half the transactions start 25 ms late, a quarter lose a conflict.
fn seeded_plan() -> FaultPlan<SqlError> {
  let conflict = SqlError::TransactionFailed(
    "injected: could not serialize access due to concurrent update (SQLSTATE 40001)".into(),
  );
  FaultPlan::seeded(7)
    .with(0.5, Fault::delay(Duration::from_millis(25)))
    .with(0.25, Fault::fail(conflict))
}

#[tokio::test]
async fn postings_survive_sql_latency_and_lost_conflicts() {
  let plan = seeded_plan();
  let schedule = plan.clone();
  with_ledger(
    move |_, services| {
      let inner = Arc::clone(&services.database.primary);
      services.database.primary = Arc::new(Faulty { inner, plan });
    },
    move |l| async move {
      let team = l.team("acme").await;
      let accounts = l.accounts("acme", "acme-accountant", 2).await;
      let (cash, sales) = (accounts[0].id, accounts[1].id);
      for _ in 0..8 {
        l.transfer(&team.accountant, cash, sales, 10).await;
      }
      // Every entry posted once: the balance and the entry count agree.
      let balance: Value = l
        .get(&format!("/accounts/{cash}/balance"), &team.viewer)
        .await
        .json()
        .unwrap();
      assert_eq!(balance["balance"], 80);
      let entries: Value = l.get("/entries", &team.viewer).await.json().unwrap();
      assert_eq!(entries["entries"].as_array().unwrap().len(), 8);

      // The plan did inject both kinds, so the retries ran.
      let history = schedule.history();
      let failed = history
        .iter()
        .filter(|f| **f == Some(FaultKind::Fail))
        .count();
      let delayed = history
        .iter()
        .filter(|f| matches!(f, Some(FaultKind::Delay(_))))
        .count();
      assert!(failed > 0 && delayed > 0, "{history:?}");
      assert_eq!(schedule.seed(), Some(7));
      // The same seed gives the same schedule: the run is reproducible.
      let again = seeded_plan();
      let replayed: Vec<Option<FaultKind>> = (0..history.len())
        .map(|_| again.next_fault().map(|f| f.kind()))
        .collect();
      assert_eq!(replayed, history);
      // The rest of the service was never affected.
      l.get("/health", &As::Anonymous)
        .await
        .assert_status(StatusCode::OK);
    },
  )
  .await;
}
