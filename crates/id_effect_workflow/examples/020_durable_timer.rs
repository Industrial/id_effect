//! A durable timer: a trial that sends a reminder one day after sign-up. The timer is a job
//! with `run_at` in the queue, not a sleeping task, so the engine that started the wait can go
//! away; another engine finishes the instance once the timer fired. A test clock
//! (`WallClock::anchored` + `jump`) makes the day pass at once. Runs in memory.
//!
//! `cargo run -p id_effect_workflow --example 020_durable_timer`

use std::process::ExitCode;
use std::time::{Duration, Instant};

use id_effect::{Effect, Exit, Schema, UtcDateTime, WallClock, hlist, succeed};
use id_effect_jobs::{
  JobQueueEnv, JobQueueService, JobRegistry, MemoryJobQueue, Worker, WorkerConfig,
};
use id_effect_workflow::{
  MemoryWorkflowStore, RunOutcome, TIMER_QUEUE, Workflow, WorkflowContext, WorkflowEngine,
  WorkflowError, WorkflowId,
};

const DAY: Duration = Duration::from_secs(24 * 3600);

#[derive(Clone, Debug, Schema)]
struct Signup {
  user: String,
}

#[derive(Clone, Debug, Schema)]
struct Reminder {
  user: String,
  signed_up: String,
  sent: String,
}

struct Trial;

impl Workflow for Trial {
  type Input = Signup;
  type Output = Reminder;
  const NAME: &'static str = "trial";

  fn run(&self, ctx: WorkflowContext, signup: Signup) -> Effect<Reminder, WorkflowError> {
    let (wait, later, send) = (ctx.clone(), ctx.clone(), ctx.clone());
    ctx.now().flat_map(move |signed_up| {
      wait.sleep(DAY).flat_map(move |()| {
        later.now().flat_map(move |sent| {
          let reminder = Reminder {
            user: signup.user,
            signed_up: signed_up.0.to_string(),
            sent: sent.0.to_string(),
          };
          send.step("send-reminder", succeed::<_, String, _>(reminder))
        })
      })
    })
  }
}

fn main() -> ExitCode {
  let clock = WallClock::anchored(Instant::now(), UtcDateTime::unsafe_make(1_700_000_000_000));
  let (store, queue) = (
    MemoryWorkflowStore::new(),
    MemoryJobQueue::new(clock.clone()),
  );
  let engine = |store: &MemoryWorkflowStore| {
    WorkflowEngine::new(store.clone())
      .with_clock(clock.clone())
      .with_job_queue(queue.clone())
      .register(Trial)
      .expect("register")
  };
  let id = WorkflowId::new("trial:ada").expect("id");

  let first = engine(&store);
  let started = first
    .start::<Trial>(id.clone(), Signup { user: "ada".into() })
    .block_on();
  assert!(matches!(started, Exit::Success(_)), "{started:?}");
  let parked = first.resume(&id).block_on();
  println!("first engine: {parked:?} (the timer is a job due in a day)");
  drop(first);

  // The workers fire due timers into the store; any engine resumes the instance.
  let second = engine(&store);
  let registry: JobRegistry<JobQueueEnv> =
    second.register_timers(JobRegistry::new()).expect("timers");
  let worker = Worker::new(registry)
    .queue(TIMER_QUEUE, 1)
    .with_config(WorkerConfig {
      clock: clock.clone(),
      ..WorkerConfig::default()
    });
  let drain = || {
    let env = hlist![JobQueueService::new(queue.clone())];
    match worker.clone().run_until_idle().provide_all(env).block_on() {
      Exit::Success(report) => report.runs(),
      Exit::Failure(cause) => panic!("worker: {cause:?}"),
    }
  };
  println!("timers fired before the day passed: {}", drain());
  clock.jump(DAY);
  println!("timers fired a day later: {}", drain());

  let done = second.resume(&id).block_on();
  println!("second engine: {done:?}");
  match second.result::<Trial>(&id).block_on() {
    Exit::Success(Some(r)) if matches!(done, Exit::Success(Some(RunOutcome::Completed))) => {
      println!(
        "reminded {} (signed up {}, reminded {})",
        r.user, r.signed_up, r.sent
      );
      ExitCode::SUCCESS
    }
    other => {
      eprintln!("unexpected result {other:?}");
      ExitCode::FAILURE
    }
  }
}
