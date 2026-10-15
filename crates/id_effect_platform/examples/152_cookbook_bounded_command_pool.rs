//! Ex 152 — Cookbook: run N external commands with at most K in flight.
//!
//! Each job runs its command through the `ProcessRuntimeService` capability from
//! `id_effect_platform::command` while holding a permit of a `next` `Semaphore` (waiting while all
//! K are taken); the permit is released however the job ends. All jobs are forked at once with
//! `for_each_par`, so the semaphore, not the caller, bounds them. No hand-rolled thread pool or
//! `Condvar` is needed. Use `id_effect_resilience::Bulkhead` instead of the semaphore when
//! surplus work should be *rejected* rather than queued.
//!
//! The "external command" is this example binary re-invoked as `child <exit-code>`, so the recipe
//! behaves the same on every OS and needs no fixtures.
//!
//! Run: `cargo run -p id_effect_platform --example 152_cookbook_bounded_command_pool`

use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use id_effect::Exit;
use id_effect::next::{Concurrency, Effect, Semaphore, for_each_par, sync};
use id_effect_platform::command::{
  CommandSpec, ProcessRuntimeEnv, TokioProcessRuntimeProvider, spawn_wait,
};
use id_effect_platform::error::ProcessError;
use id_effect_tokio::TokioRuntime;

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

/// One job: run `cmd` holding a permit, yield the exit code.
fn bounded_job(
  sem: &Semaphore,
  gauge: Gauge,
  cmd: CommandSpec,
) -> Effect<i32, ProcessError, ProcessRuntimeEnv> {
  let (g_in, g_out) = (gauge.clone(), gauge);
  let job = sync(move || g_in.enter())
    .then(spawn_wait(cmd))
    .map(move |status| {
      g_out.exit();
      status.code().unwrap_or(-1)
    });
  sem.with_permit(job)
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
  let sem = Semaphore::new(MAX_IN_FLIGHT);
  let gauge = Gauge::default();

  // Every third command "fails" with exit code 3.
  let expected: Vec<i32> = (0..JOBS).map(|i| if i % 3 == 2 { 3 } else { 0 }).collect();

  let jobs = for_each_par(
    expected.clone(),
    |code| {
      let cmd = CommandSpec::new(&exe).arg("child").arg(code.to_string());
      bounded_job(&sem, gauge.clone(), cmd)
    },
    Concurrency::Unbounded,
  );
  // Capabilities are provided once, at the edge, by the process layer.
  let program = jobs.provide_layer(TokioProcessRuntimeProvider::layer());
  let rt = TokioRuntime::current().expect("tokio runtime");
  let codes = match rt.run(program).await {
    Exit::Success(codes) => codes,
    Exit::Failure(c) => panic!("jobs failed: {c:?}"),
  };

  let peak = gauge.peak.load(Ordering::SeqCst);
  println!("exit codes {codes:?}; peak in flight {peak} (limit {MAX_IN_FLIGHT})");
  assert_eq!(codes, expected);
  assert!((1..=MAX_IN_FLIGHT).contains(&peak));
  println!("152_cookbook_bounded_command_pool ok");
  ExitCode::SUCCESS
}
