//! Background jobs on the in-memory queue and virtual time: no database needed.
//!
//! Enqueues jobs with a unique key, two priorities and a later `run_at`, drains them with a
//! worker serving two queues, and follows a failing job through its retries to `dead`.
//! `MemoryJobQueue` keeps the ordering, uniqueness, retry and lease rules of `PgJobQueue`: swap
//! it for `PgJobQueue::new(client)` (after `migrator()`) and the same program runs on PostgreSQL.
//!
//! `cargo run -p id_effect_jobs --example 010_jobs`

use std::sync::{Arc, Mutex};
use std::time::Duration;

use id_effect::{
  Effect, Executor, Exit, Never, Schedule, Schema, TestExecutor, UtcDateTime, WallClock, fail,
  hlist, sync,
};
use id_effect_jobs::{
  DrainReport, Enqueued, Job, JobError, JobFailure, JobFilter, JobOptions, JobQueue, JobQueueEnv,
  JobQueueExt, JobQueueService, JobRegistry, JobState, MemoryJobQueue, Priority, UniqueKey, Worker,
  WorkerConfig, WorkerId,
};

/// A receipt mail, on the `mail` queue: one per order while it is pending.
#[derive(Clone, Schema)]
struct SendReceipt {
  order: i64,
}

impl Job for SendReceipt {
  const NAME: &'static str = "shop.send_receipt";
  const QUEUE: &'static str = "mail";

  fn unique_key(&self) -> Option<UniqueKey> {
    Some(UniqueKey::while_pending(format!("receipt-{}", self.order)))
  }
}

/// A report, on the `default` queue; its priority is chosen per enqueue.
#[derive(Clone, Schema)]
struct BuildReport {
  name: String,
}

impl Job for BuildReport {
  const NAME: &'static str = "shop.build_report";
}

/// A warehouse sync that always fails: 3 attempts, 1 s then 2 s apart.
#[derive(Clone, Schema)]
struct SyncWarehouse {
  sku: String,
}

impl Job for SyncWarehouse {
  const NAME: &'static str = "shop.sync_warehouse";

  fn max_attempts(&self) -> u16 {
    3
  }

  fn retry() -> Schedule<(), JobFailure> {
    Schedule::exponential(Duration::from_secs(1)).map(|_| ())
  }
}

/// The queue on a test executor: every effect runs to completion on virtual time.
struct App {
  exec: TestExecutor,
  clock: WallClock,
  store: MemoryJobQueue,
}

impl App {
  fn new() -> Self {
    let exec = TestExecutor::new();
    // 2030-01-01T00:00:00Z.
    let clock = WallClock::anchored(exec.now(), UtcDateTime::unsafe_make(1_893_456_000_000));
    Self {
      store: MemoryJobQueue::new(clock.clone()),
      clock,
      exec,
    }
  }

  fn run<A: Send + 'static>(&self, eff: Effect<A, JobError>) -> A {
    let slot = Arc::new(Mutex::new(None));
    let out = Arc::clone(&slot);
    let fiber = eff
      .exit::<Never>()
      .map(move |exit| *out.lock().unwrap() = Some(exit))
      .fork_root(Arc::new(self.exec.clone()));
    self.exec.run_until_stalled();
    assert!(fiber.is_done());
    match slot.lock().unwrap().take() {
      Some(Exit::Success(a)) => a,
      Some(Exit::Failure(cause)) => panic!("failed: {cause:?}"),
      None => panic!("no exit"),
    }
  }

  /// Runs `worker` until no job is runnable at the clock's time.
  fn drain(&self, worker: &Worker<JobQueueEnv>) -> DrainReport {
    let env = hlist![JobQueueService::new(self.store.clone())];
    self.run(worker.clone().run_until_idle().provide_all(env))
  }

  fn only(&self, state: JobState) -> Vec<id_effect_jobs::JobRecord> {
    let filter = JobFilter {
      state: Some(state),
      ..JobFilter::default()
    };
    self.run(self.store.list(filter))
  }
}

fn main() {
  let app = App::new();
  let log = Arc::new(Mutex::new(Vec::<String>::new()));

  let (l1, l2) = (Arc::clone(&log), Arc::clone(&log));
  let registry = JobRegistry::<JobQueueEnv>::new()
    .register::<SendReceipt, JobFailure>(move |job, ctx| {
      let log = Arc::clone(&l1);
      sync(move || {
        log
          .lock()
          .unwrap()
          .push(format!("{}: receipt {}", ctx.queue, job.order))
      })
    })
    .and_then(|r| {
      r.register::<BuildReport, JobFailure>(move |job, ctx| {
        let log = Arc::clone(&l2);
        sync(move || {
          log
            .lock()
            .unwrap()
            .push(format!("{}: report {}", ctx.queue, job.name))
        })
      })
    })
    .and_then(|r| {
      r.register::<SyncWarehouse, String>(|job, ctx| {
        fail(format!(
          "warehouse down ({}, attempt {})",
          job.sku, ctx.attempt
        ))
      })
    })
    .expect("distinct job names");
  let config = WorkerConfig {
    id: WorkerId::new("example"),
    clock: app.clock.clone(),
    ..WorkerConfig::default()
  };
  // One attempt at a time on `default` (so the priorities show), two on `mail`.
  let worker = Worker::new(registry)
    .queue("default", 1)
    .queue("mail", 2)
    .with_config(config);

  // A unique key deduplicates while the first job is pending.
  let first = app.run(
    app
      .store
      .enqueue(SendReceipt { order: 1 }, JobOptions::new()),
  );
  let again = app.run(
    app
      .store
      .enqueue(SendReceipt { order: 1 }, JobOptions::new()),
  );
  assert!(first.is_new());
  assert_eq!(
    again,
    Enqueued::Duplicate {
      existing: first.id()
    }
  );
  println!("enqueued receipt 1 ({first:?}), again: {again:?}");

  // Higher priority runs first within a queue; `run_at` holds a job back.
  for (name, priority) in [("daily", Priority::LOW), ("audit", Priority::HIGH)] {
    let opts = JobOptions::new().priority(priority);
    app.run(app.store.enqueue(BuildReport { name: name.into() }, opts));
  }
  let weekly = BuildReport {
    name: "weekly".into(),
  };
  app.run(app.store.enqueue_after(weekly, Duration::from_secs(3600)));
  app.run(app.store.enqueue(
    SyncWarehouse {
      sku: "SKU-1".into(),
    },
    JobOptions::new(),
  ));

  let report = app.drain(&worker);
  println!("first drain: {report:?}");
  assert_eq!((report.succeeded, report.retrying), (3, 1));
  let reports: Vec<String> = log
    .lock()
    .unwrap()
    .iter()
    .filter(|line| line.starts_with("default:"))
    .cloned()
    .collect();
  assert_eq!(reports, ["default: report audit", "default: report daily"]);

  // The failing job runs again after each backoff, then is dead.
  app.exec.advance(Duration::from_secs(1));
  assert_eq!(app.drain(&worker).retrying, 1);
  app.exec.advance(Duration::from_secs(2));
  assert_eq!(app.drain(&worker).dead, 1);
  let dead = app.only(JobState::Dead).remove(0);
  println!(
    "dead after {} attempts: {}",
    dead.attempts,
    dead.last_error.as_deref().unwrap_or_default()
  );
  assert_eq!(dead.attempts, 3);

  // The weekly report waits for its `run_at`.
  assert_eq!(app.only(JobState::Scheduled).len(), 1);
  app.exec.advance(Duration::from_secs(3600));
  assert_eq!(app.drain(&worker).succeeded, 1);

  // A dead job can be retried by hand (or cancelled before it runs).
  assert!(app.run(app.store.retry_now(dead.id)));
  assert!(app.run(app.store.cancel(dead.id)));
  for stats in app.run(app.store.stats()) {
    println!("queue {:<8} {:?}", stats.queue, stats.counts);
  }

  println!("ran:");
  for line in log.lock().unwrap().iter() {
    println!("  {line}");
  }
  println!("states:");
  for state in JobState::ALL {
    let kind = if state.is_terminal() {
      "terminal"
    } else {
      "active"
    };
    println!("  {state:<10} {kind}");
  }
}
