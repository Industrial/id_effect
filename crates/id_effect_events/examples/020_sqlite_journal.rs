//! Ex 020 — the durable `SqlEventJournal` on SQLite, with projections rebuilt in dependency
//! order. The same journal runs on PostgreSQL with `PgSqlClient` (feature `pg`).
//!
//! ```bash
//! cargo run -p id_effect_events --example 020_sqlite_journal --features sqlite
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use id_effect::{Effect, Exit};
use id_effect_events::{EventStore, Projection, ProjectionNode, ProjectionRunner, SqlEventJournal};
use id_effect_sql::sqlite::{SqlitePoolConfig, SqliteSqlClient, sqlite_pool_from_config};
use id_effect_tokio::TokioRuntime;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
enum OrderEvent {
  Placed { sku: String, qty: u32 },
  Cancelled { sku: String },
}

/// Counts units placed (`units`) or cancellations (`cancellations`), by projection id.
struct Counter(&'static str);
impl Projection<u32, OrderEvent> for Counter {
  fn initial(&self) -> u32 {
    0
  }
  fn apply(&self, n: u32, event: &OrderEvent) -> u32 {
    match (self.0, event) {
      ("units", OrderEvent::Placed { qty, .. }) => n + qty,
      ("cancellations", OrderEvent::Cancelled { .. }) => n + 1,
      _ => n,
    }
  }
}

async fn run<A: Send + 'static, E: Send + std::fmt::Debug + 'static>(eff: Effect<A, E>) -> A {
  match TokioRuntime::current().expect("tokio").run(eff).await {
    Exit::Success(a) => a,
    Exit::Failure(cause) => panic!("example failed: {cause:?}"),
  }
}

#[tokio::main]
async fn main() {
  let pool = sqlite_pool_from_config(SqlitePoolConfig::memory())
    .await
    .expect("sqlite");
  let journal = SqlEventJournal::<OrderEvent>::new(Arc::new(SqliteSqlClient::new(pool)));
  run(journal.create_table()).await;

  let placed = run(journal.append_expected(
    "order-7",
    0,
    &[
      OrderEvent::Placed {
        sku: "widget".into(),
        qty: 3,
      },
      OrderEvent::Placed {
        sku: "gadget".into(),
        qty: 1,
      },
    ],
  ))
  .await;
  let at = placed.last().map_or(0, |e| e.version);
  run(journal.append_expected(
    "order-7",
    at,
    &[OrderEvent::Cancelled {
      sku: "gadget".into(),
    }],
  ))
  .await;

  let mut runner = ProjectionRunner::new();
  runner.register(ProjectionNode::new("units", Vec::<&str>::new()));
  runner.register(ProjectionNode::new("cancellations", ["units"]));
  let projections = HashMap::from([
    ("units".to_owned(), Counter("units")),
    ("cancellations".to_owned(), Counter("cancellations")),
  ]);
  let rebuilt = run(runner.run_all(&journal, "order-7", 1, projections)).await;
  for (id, value) in &rebuilt {
    println!("{id}: {value}");
  }
  assert_eq!(
    rebuilt,
    [("units".to_owned(), 4), ("cancellations".to_owned(), 1)]
  );
  println!(
    "stream at version {}",
    run(journal.latest_version("order-7")).await
  );
}
