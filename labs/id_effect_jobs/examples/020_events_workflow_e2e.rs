//! End-to-end: SQL journal events → ordered projections → duroxide step.
//!
//! ```bash
//! DATABASE_URL=postgres://... cargo run -p id_effect_jobs --example 020_events_workflow_e2e \
//!   --features "memory,postgres" \
//!   --manifest-path crates/id_effect_jobs/Cargo.toml
//! ```
//!
//! Requires dev-dependencies (`id_effect_events` / `id_effect_workflow` with PG features).

#[cfg(not(feature = "postgres"))]
fn main() {
  eprintln!("enable features: memory, postgres (and the duroxide dev-dependency)");
}

#[cfg(feature = "postgres")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
  use id_effect_events::{
    EventStore, Projection, ProjectionNode, ProjectionRunner, SqlEventJournal,
  };
  use id_effect_sql::pg::{PgPoolConfig, PgSqlClient, pg_pool_from_config};
  use id_effect_tokio::TokioRuntime;
  use id_effect_workflow::{DuroxideStepJournal, StepJournal, bootstrap_duroxide_schema};
  use std::collections::HashMap;
  use std::sync::Arc;

  let url = match std::env::var("DATABASE_URL") {
    Ok(u) => u,
    Err(_) => {
      println!("020_events_workflow_e2e: skip (DATABASE_URL unset)");
      return Ok(());
    }
  };

  let pool = pg_pool_from_config(PgPoolConfig::from_url(url.clone()).with_max_size(4))
    .await
    .map_err(|e| e.0.to_string())?;

  let rt = TokioRuntime::current()?;
  let store = SqlEventJournal::<OrderEvt>::new(Arc::new(PgSqlClient::new(pool.clone())));
  rt.run(store.create_table())
    .await
    .into_result()
    .map_err(|c| format!("journal ddl: {c:?}"))?;
  bootstrap_duroxide_schema(&pool).await?;

  #[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
  enum OrderEvt {
    Placed { sku: String },
  }

  struct OrderSummary;
  impl Projection<String, OrderEvt> for OrderSummary {
    fn initial(&self) -> String {
      String::new()
    }
    fn apply(&self, _state: String, event: &OrderEvt) -> String {
      match event {
        OrderEvt::Placed { sku } => format!("sku={sku}"),
      }
    }
  }

  let stream = format!("order-{}", uuid::Uuid::new_v4());
  rt.run(store.append(
    &stream,
    &[OrderEvt::Placed {
      sku: "widget".into(),
    }],
  ))
  .await
  .into_result()
  .map_err(|c| format!("append: {c:?}"))?;

  let mut runner = ProjectionRunner::new();
  runner.register(ProjectionNode::new("summary", Vec::<&str>::new()));
  let mut projections = HashMap::new();
  projections.insert("summary".to_string(), OrderSummary);
  let rebuilt = rt
    .run(runner.run_all(&store, &stream, 1, projections))
    .await
    .into_result()
    .map_err(|c| format!("projections: {c:?}"))?;
  assert_eq!(rebuilt[0].1, "sku=widget");
  println!("projection: {}", rebuilt[0].1);

  // `StepJournal` is a synchronous edge: drive it from a plain thread, off the async runtime.
  let step = std::thread::spawn(move || {
    let wf = format!("wf-{}", uuid::Uuid::new_v4());
    let mut journal = DuroxideStepJournal::new(pool);
    journal.register_workflow(&wf)?;
    journal.run_step_typed::<String, _>(&wf, 0, "after_events", || Ok(format!("done:{stream}")))
  })
  .join()
  .map_err(|_| "duroxide step thread panicked")??;
  println!("duroxide step: {step}");

  Ok(())
}
