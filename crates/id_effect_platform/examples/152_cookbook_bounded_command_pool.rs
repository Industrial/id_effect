//! Ex 152 — Cookbook: run N external commands with at most K in flight.
//!
//! Each job takes a permit from a core `Semaphore` (waiting while all K are taken), runs its
//! command through the `ProcessRuntimeService` capability from `id_effect_platform::command`, and
//! releases the permit when it finishes. No hand-rolled thread pool or `Condvar` is needed. Use
//! `id_effect_resilience::Bulkhead` instead of the semaphore when surplus work should be
//! *rejected* rather than queued.
//!
//! The "external command" is this example binary re-invoked as `child <exit-code>`, so the recipe
//! behaves the same on every OS and needs no fixtures.
//!
//! Run: `cargo run -p id_effect_platform --example 152_cookbook_bounded_command_pool`

use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use id_effect::{
  Effect, Needs, Semaphore, box_future, build_env, provide, run_async, run_blocking,
};
use id_effect_platform::command::{
  CommandSpec, ProcessRuntimeService, TokioProcessRuntimeProvider, spawn_wait,
};
use id_effect_platform::error::ProcessError;

/// How many commands may run at once.
const MAX_IN_FLIGHT: usize = 3;
/// How many commands to run in total.
const JOBS: i32 = 8;

/// Tracks how many jobs are inside the permit, and the highest that number ever got.
#[derive(Clone, Default)]
struct Gauge {
  now: Arc<AtomicUsize>,
  peak: Arc<AtomicUsize>,
}

impl Gauge {
  fn enter(&self) {
    let now = self.now.fetch_add(1, Ordering::SeqCst) + 1;
    self.peak.fetch_max(now, Ordering::SeqCst);
  }

  fn exit(&self) {
    self.now.fetch_sub(1, Ordering::SeqCst);
  }
}

/// One job: wait for a permit, run `cmd`, release the permit, yield the exit code.
fn bounded_job<R>(sem: Semaphore, gauge: Gauge, cmd: CommandSpec) -> Effect<i32, ProcessError, R>
where
  R: Needs<ProcessRuntimeService> + 'static,
{
  Effect::new_async(move |r: &mut R| {
    let run = spawn_wait::<R>(cmd);
    box_future(async move {
      // `acquire_owned` waits for a permit; dropping `_permit` gives it back, on every path.
      let _permit = match run_async(sem.acquire_owned(), ()).await {
        Ok(permit) => permit,
        Err(never) => match never {},
      };
      gauge.enter();
      let status = run.run(r).await;
      gauge.exit();
      Ok(status?.code().unwrap_or(-1))
    })
  })
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
  let mut args = std::env::args().skip(1);
  if args.next().as_deref() == Some("child") {
    // Child mode: behave like an external tool that exits with the requested code.
    let code: u8 = args.next().and_then(|c| c.parse().ok()).unwrap_or(0);
    return ExitCode::from(code);
  }

  let exe = std::env::current_exe().expect("current_exe");
  let sem = run_blocking(Semaphore::make(MAX_IN_FLIGHT), ()).expect("semaphore");
  let gauge = Gauge::default();
  // Capabilities are provided once, at the edge.
  let env = build_env([provide!(TokioProcessRuntimeProvider)]).expect("env");

  // Every third command "fails" with exit code 3.
  let expected: Vec<i32> = (0..JOBS).map(|i| if i % 3 == 2 { 3 } else { 0 }).collect();

  // Start all jobs concurrently on this thread; the semaphore, not the caller, bounds them.
  let local = tokio::task::LocalSet::new();
  let codes: Vec<i32> = local
    .run_until(async {
      let handles: Vec<_> = expected
        .iter()
        .map(|code| {
          let cmd = CommandSpec::new(&exe).arg("child").arg(code.to_string());
          let job = bounded_job(sem.clone(), gauge.clone(), cmd);
          tokio::task::spawn_local(run_async(job, env.clone()))
        })
        .collect();
      let mut codes = Vec::with_capacity(handles.len());
      for handle in handles {
        codes.push(handle.await.expect("join").expect("spawn"));
      }
      codes
    })
    .await;

  let peak = gauge.peak.load(Ordering::SeqCst);
  println!("exit codes {codes:?}; peak in flight {peak} (limit {MAX_IN_FLIGHT})");
  assert_eq!(codes, expected);
  assert!((1..=MAX_IN_FLIGHT).contains(&peak));
  println!("152_cookbook_bounded_command_pool ok");
  ExitCode::SUCCESS
}
