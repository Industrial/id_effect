//! A durable order saga: reserve stock, then charge the card; when the charge is declined the
//! reservation is released by its compensation, a journaled activity. Runs in memory
//! (`MemoryWorkflowStore`, `MemoryJobQueue`); with `PgWorkflowStore` and `PgJobQueue` the same
//! code survives restarts (`tests/saga_pg.rs`).
//!
//! `cargo run -p id_effect_workflow --example 010_order_saga`

use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use id_effect::{Effect, Exit, Schema, WallClock, fail, hlist, succeed, sync};
use id_effect_jobs::{
  JobQueueEnv, JobQueueService, JobRegistry, MemoryJobQueue, Worker, WorkerConfig,
};
use id_effect_workflow::{
  ACTIVITY_QUEUE, Activity, ActivityContext, JobFailure, MemoryWorkflowStore, Workflow,
  WorkflowContext, WorkflowEngine, WorkflowError, WorkflowId, WorkflowStatus,
};

#[derive(Clone, Debug, Schema)]
struct Order {
  id: i64,
  units: i64,
  cents: i64,
}

#[derive(Clone, Debug, Schema)]
struct Reservation {
  order: i64,
  units: i64,
}

type Log = Arc<Mutex<Vec<String>>>;

fn say(log: &Log, line: String) -> Effect<(), JobFailure> {
  let log = Arc::clone(log);
  sync(move || {
    println!("  {line}");
    log.lock().unwrap().push(line);
  })
}

struct ReserveStock(Log);

impl Activity for ReserveStock {
  type Input = Order;
  type Output = Reservation;
  const NAME: &'static str = "reserve-stock";

  fn run(&self, _: ActivityContext, order: Order) -> Effect<Reservation, JobFailure> {
    let reservation = Reservation {
      order: order.id,
      units: order.units,
    };
    say(&self.0, format!("reserved {} units", order.units)).map(move |()| reservation)
  }
}

struct ReleaseStock(Log);

impl Activity for ReleaseStock {
  type Input = Reservation;
  type Output = Reservation;
  const NAME: &'static str = "release-stock";

  fn run(&self, _: ActivityContext, r: Reservation) -> Effect<Reservation, JobFailure> {
    say(&self.0, format!("released {} units", r.units)).map(move |()| r)
  }
}

/// Declines anything over 100.00.
struct ChargeCard;

impl Activity for ChargeCard {
  type Input = Order;
  type Output = Order;
  const NAME: &'static str = "charge-card";
  const MAX_ATTEMPTS: u16 = 1;

  fn run(&self, _: ActivityContext, order: Order) -> Effect<Order, JobFailure> {
    if order.cents > 10_000 {
      return fail(JobFailure::discard("card declined"));
    }
    succeed(order)
  }
}

struct PlaceOrder;

impl Workflow for PlaceOrder {
  type Input = Order;
  type Output = Order;
  const NAME: &'static str = "place-order";

  fn run(&self, ctx: WorkflowContext, order: Order) -> Effect<Order, WorkflowError> {
    let charge = ctx.clone();
    ctx.saga(move |saga| {
      saga
        // On failure later on, `release-stock` runs with this reservation.
        .activity::<ReserveStock, ReleaseStock, _>(order.clone(), Reservation::clone)
        .flat_map(move |_| charge.activity::<ChargeCard>(order))
    })
  }
}

fn main() -> ExitCode {
  let log = Log::default();
  let clock = WallClock::system();
  let (store, queue) = (
    MemoryWorkflowStore::new(),
    MemoryJobQueue::new(clock.clone()),
  );
  let engine = WorkflowEngine::new(store)
    .with_job_queue(queue.clone())
    .register(PlaceOrder)
    .expect("register");
  let registry: JobRegistry<JobQueueEnv> = JobRegistry::new();
  let registry = engine
    .register_activity(registry, ReserveStock(Arc::clone(&log)))
    .and_then(|r| engine.register_activity(r, ReleaseStock(Arc::clone(&log))))
    .and_then(|r| engine.register_activity(r, ChargeCard))
    .expect("activities");
  let worker = Worker::new(registry)
    .queue(ACTIVITY_QUEUE, 2)
    .with_config(WorkerConfig {
      clock,
      ..WorkerConfig::default()
    });

  // The second order is over the card limit: its reservation is released.
  for (order, expected) in [
    (
      Order {
        id: 1,
        units: 2,
        cents: 4_000,
      },
      WorkflowStatus::Completed,
    ),
    (
      Order {
        id: 2,
        units: 5,
        cents: 25_000,
      },
      WorkflowStatus::Failed,
    ),
  ] {
    println!("order {} ({} cents):", order.id, order.cents);
    let id = WorkflowId::new(format!("order-{}", order.id)).expect("id");
    let started = engine.start::<PlaceOrder>(id.clone(), order).block_on();
    assert!(matches!(started, Exit::Success(_)), "{started:?}");
    // Drive the instance: every run parks on an activity until a worker has run it.
    let status = loop {
      let _ = engine.resume(&id).block_on();
      match engine.status(&id).block_on() {
        Exit::Success(status) if status.is_terminal() => break status,
        Exit::Success(_) => {}
        Exit::Failure(cause) => {
          eprintln!("status: {cause:?}");
          return ExitCode::FAILURE;
        }
      }
      let env = hlist![JobQueueService::new(queue.clone())];
      let _ = worker.clone().run_until_idle().provide_all(env).block_on();
    };
    match engine.result::<PlaceOrder>(&id).block_on() {
      Exit::Success(_) => println!("  {status}"),
      Exit::Failure(cause) => println!("  {status}: {cause:?}"),
    }
    if status != expected {
      return ExitCode::FAILURE;
    }
  }
  let expected = ["reserved 2 units", "reserved 5 units", "released 5 units"];
  if *log.lock().unwrap() != expected {
    eprintln!("unexpected activity log {:?}", log.lock().unwrap());
    return ExitCode::FAILURE;
  }
  ExitCode::SUCCESS
}
