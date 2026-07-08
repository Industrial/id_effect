//! End-to-end: es-entity events → graph projections → obix outbox → duroxide step.
//!
//! ```bash
//! DATABASE_URL=postgres://... cargo run -p id_effect_jobs --example 020_events_workflow_e2e \
//!   --features "memory,obix,apalis" \
//!   --manifest-path crates/id_effect_jobs/Cargo.toml
//! ```
//!
//! Requires dev-dependencies (`id_effect_events` / `id_effect_workflow` with PG features).

#[cfg(not(all(feature = "obix", feature = "postgres")))]
fn main() {
  eprintln!("enable features: memory, obix, postgres (and dev-deps for es-entity/duroxide)");
}

#[cfg(all(feature = "obix", feature = "postgres"))]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
  use id_effect::run_async;
  use id_effect_events::{
    EsEntityEventStore, EsEntityPgBackend, EventStore, Projection, ProjectionNode,
    ProjectionRunner, apply_es_entity_journal_ddl,
  };
  use id_effect_jobs::ObixOutbox;
  use id_effect_sql_pg::{PgPoolConfig, pg_pool_from_config};
  use id_effect_workflow::{DuroxideStepJournal, StepJournal, bootstrap_duroxide_schema};
  use obix::MailboxConfig;
  use std::collections::HashMap;

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

  apply_es_entity_journal_ddl(&pool).await?;
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
  let store = EsEntityEventStore::<OrderEvt>::new(EsEntityPgBackend::new(pool.clone()));
  run_async(
    store.append(
      &stream,
      &[OrderEvt::Placed {
        sku: "widget".into(),
      }],
    ),
    (),
  )
  .await?;

  let mut runner = ProjectionRunner::new();
  runner.register(ProjectionNode::new("summary", Vec::<&str>::new()));
  let mut projections = HashMap::new();
  projections.insert("summary".to_string(), OrderSummary);
  let rebuilt = runner.run_all(&store, &stream, 1, &projections).await?;
  assert_eq!(rebuilt[0].1, "sku=widget");
  println!("projection: {}", rebuilt[0].1);

  let config = MailboxConfig::builder().build()?;
  let obix = ObixOutbox::init(&pool, config).await?;
  run_async(
    obix.insert(id_effect_jobs::OutboxRecord::new(
      &stream,
      "OrderPlaced",
      br#"{"sku":"widget"}"#,
    )),
    (),
  )
  .await?;
  println!("obix outbox row inserted");

  let wf = format!("wf-{}", uuid::Uuid::new_v4());
  let mut journal = DuroxideStepJournal::new(pool);
  journal.register_workflow(&wf)?;
  let step: String =
    journal.run_step_typed(&wf, 0, "after_events", || Ok(format!("done:{stream}")))?;
  println!("duroxide step: {step}");

  Ok(())
}
