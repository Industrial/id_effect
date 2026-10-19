//! A signal: an expense waits for a manager's decision, sent by another process through a
//! `WorkflowClient`. The signal is stored (buffered if it arrives before the wait) and wakes the
//! parked instance. With `DATABASE_URL` set it runs on PostgreSQL (the workflow tables are
//! migrated in the database's default schema; each run uses a fresh instance id); without it, in
//! memory.
//!
//! `DATABASE_URL=postgres://… cargo run -p id_effect_workflow --example 030_signal_approval`

use std::process::ExitCode;
use std::sync::Arc;

use id_effect::{Effect, Exit, Schema, succeed};
use id_effect_sql::pg::{PgPoolConfig, PgSqlClient, pg_pool_from_config};
use id_effect_tokio::TokioRuntime;
use id_effect_workflow::{
  MemoryWorkflowStore, PgWorkflowStore, RunOutcome, Workflow, WorkflowClient, WorkflowContext,
  WorkflowEngine, WorkflowError, WorkflowId, WorkflowStore, migrator,
};

#[derive(Clone, Debug, Schema)]
struct Expense {
  cents: i64,
}

#[derive(Clone, Debug, Schema)]
struct Decision {
  approved: bool,
  by: String,
}

struct Approval;

impl Workflow for Approval {
  type Input = Expense;
  type Output = Decision;
  const NAME: &'static str = "expense-approval";

  fn run(&self, ctx: WorkflowContext, expense: Expense) -> Effect<Decision, WorkflowError> {
    let wait = ctx.clone();
    ctx
      .step("submit", succeed::<_, String, _>(expense))
      .flat_map(move |_| wait.wait_signal::<Decision>("decision"))
  }
}

fn run<A: Send + 'static>(rt: &TokioRuntime, eff: Effect<A, WorkflowError>) -> Result<A, String> {
  match rt.block_on(rt.run(eff)) {
    Exit::Success(a) => Ok(a),
    Exit::Failure(cause) => Err(format!("{cause:?}")),
  }
}

/// The store: PostgreSQL at `DATABASE_URL` (migrated), or memory.
fn store(rt: &TokioRuntime) -> Result<Arc<dyn WorkflowStore>, String> {
  let Ok(url) = std::env::var("DATABASE_URL") else {
    println!("store: memory (set DATABASE_URL for PostgreSQL)");
    return Ok(Arc::new(MemoryWorkflowStore::new()));
  };
  let pool = rt
    .block_on(pg_pool_from_config(PgPoolConfig::from_url(url)))
    .map_err(|e| format!("cannot connect: {}", e.0))?;
  let client = PgSqlClient::new(pool);
  let applied = rt.block_on(rt.run(migrator().run_on(Arc::new(client.clone()))));
  if let Exit::Failure(cause) = applied {
    return Err(format!("migrate: {cause:?}"));
  }
  println!("store: PostgreSQL");
  Ok(Arc::new(PgWorkflowStore::new(client)))
}

fn main() -> ExitCode {
  match approve() {
    Ok(()) => ExitCode::SUCCESS,
    Err(e) => {
      eprintln!("030_signal_approval: {e}");
      ExitCode::FAILURE
    }
  }
}

fn approve() -> Result<(), String> {
  let rt = TokioRuntime::new_multi_thread().map_err(|e| format!("runtime: {e:?}"))?;
  let store = store(&rt)?;
  let engine = WorkflowEngine::from_arc(Arc::clone(&store))
    .register(Approval)
    .map_err(|e| e.to_string())?;
  let id = WorkflowId::random();
  run(
    &rt,
    engine.start::<Approval>(id.clone(), Expense { cents: 12_900 }),
  )?;
  let parked = run(&rt, engine.resume(&id))?;
  println!("{id}: {parked:?} for a decision");

  // Another process: a client over the same store sends the decision.
  let manager = WorkflowClient::from_arc(store);
  let decision = Decision {
    approved: true,
    by: "grace".into(),
  };
  let sent = run(&rt, manager.signal(&id, "decision", decision))?;
  println!("decision sent: {sent}");

  let done = run(&rt, engine.resume(&id))?;
  let decision = run(&rt, engine.result::<Approval>(&id))?;
  match (done, decision) {
    (Some(RunOutcome::Completed), Some(d)) => {
      println!("approved: {} (by {})", d.approved, d.by);
      Ok(())
    }
    other => Err(format!("unexpected outcome {other:?}")),
  }
}
