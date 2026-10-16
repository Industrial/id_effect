//! Ex 123 — named cron jobs: a central `CronRegistry`, a time zone with a DST transition, an
//! overlap policy, a missed-run catch-up, success / failure hooks and run history.
//!
//! The scheduler runs on a `TestExecutor`, so three virtual days pass instantly; with
//! `WallClock::system()` on the Tokio runtime the same registry runs in real time.
//!
//! Run: `cargo run -p id_effect --example 123_cron_jobs`

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use id_effect::{
  Cron, CronJob, CronOutcome, CronRegistry, CronRun, Effect, Executor, MissedRunPolicy,
  OverlapPolicy, TestExecutor, UtcDateTime, WallClock, fail, sleep, succeed, sync, timezone,
};

fn report() -> Effect<(), &'static str> {
  sleep(Duration::from_secs(5))
}

fn main() {
  let new_york = timezone::from_str("America/New_York").expect("tzdb");
  let sync_calls = Arc::new(AtomicU32::new(0));
  let calls = Arc::clone(&sync_calls);
  let failures = Arc::new(AtomicU32::new(0));
  let failed = Arc::clone(&failures);

  // One declarative table of jobs.
  let registry = CronRegistry::new()
    // 02:30 New York time; on 2026-03-08 02:30 does not exist and the job runs at 03:30 EDT.
    .with(CronJob::new(
      "nightly-report",
      Cron::parse("30 2 * * *").expect("cron").in_zone(new_york),
      report,
    ))
    // Every 6 hours; a run that is missed (the process was suspended) is caught up.
    .and_then(|r| {
      r.with(
        CronJob::new(
          "sync",
          Cron::parse("0 */6 * * *").expect("cron"),
          move || match calls.fetch_add(1, Ordering::SeqCst) {
            2 => fail("upstream 503"),
            _ => succeed(()),
          },
        )
        .missed_runs(MissedRunPolicy::CatchUp { max: 3 })
        .overlap(OverlapPolicy::Skip)
        .on_failure(move |run: &CronRun| {
          let (failed, run) = (Arc::clone(&failed), run.clone());
          sync(move || {
            failed.fetch_add(1, Ordering::SeqCst);
            println!(
              "  ALERT {} at {}: {:?}",
              run.job,
              run.scheduled.format_iso(),
              run.outcome
            );
          })
        }),
      )
    })
    .and_then(|r| r.schedule("heartbeat", "@hourly", || succeed::<(), &str, _>(())))
    .expect("registry");

  let exec = TestExecutor::new();
  let start = UtcDateTime("2026-03-07T12:00:00Z".parse().expect("timestamp"));
  let clock = WallClock::anchored(exec.now(), start);
  let scheduler = registry.scheduler();
  let fiber = registry
    .run(clock.clone())
    .fork_root(Arc::new(exec.clone()));

  exec.advance(Duration::from_secs(36 * 3600));
  // The host sleeps for 13 hours: two `sync` fire times are missed and caught up on wake-up.
  clock.jump(Duration::from_secs(13 * 3600));
  exec.advance(Duration::from_secs(36 * 3600));
  fiber.interrupt_now();

  for name in scheduler.job_names() {
    let runs = scheduler.history(&name).expect("job");
    let ok = runs.iter().filter(|r| r.is_success()).count();
    println!("{name}: {} records, {ok} succeeded", runs.len());
  }
  println!("nightly-report fire times:");
  for run in scheduler.history("nightly-report").expect("job") {
    println!("  {}", run.scheduled.format_iso());
  }
  println!("sync history:");
  for run in scheduler.history("sync").expect("job") {
    let late = match (run.started, &run.outcome) {
      (Some(at), CronOutcome::Succeeded | CronOutcome::Failed(_)) => {
        let at = at.to_epoch_millis() - run.scheduled.to_epoch_millis();
        format!("started {} min late", at / 60_000)
      }
      _ => String::new(),
    };
    println!("  {} {:?} {late}", run.scheduled.format_iso(), run.outcome);
  }
  assert_eq!(failures.load(Ordering::SeqCst), 1);
  assert_eq!(
    scheduler.history("nightly-report").expect("job")[0]
      .scheduled
      .format_iso(),
    "2026-03-08T03:30:00-04:00[America/New_York]"
  );
}
