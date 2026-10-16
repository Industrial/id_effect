//! Cron jobs on `TestExecutor` virtual time: DST transitions, missed runs, overlap, hooks, run
//! history, the registry and its layer, and `Schedule::cron`.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use id_effect::{
  Cron, CronError, CronJob, CronOutcome, CronRegistry, CronRun, CronScheduler, CronSkip, Effect,
  Executor, HCons, HNil, MissedRunPolicy, Never, OverlapPolicy, Schedule, TestExecutor,
  UtcDateTime, WallClock, ZonedDateTime, fail, need, sleep, succeed, sync, timezone,
};

const HOUR: Duration = Duration::from_secs(3600);
const DAY: Duration = Duration::from_secs(86_400);

fn utc(s: &str) -> UtcDateTime {
  UtcDateTime(s.parse().expect("timestamp"))
}

fn new_york() -> jiff::tz::TimeZone {
  timezone::from_str("America/New_York").expect("tzdb has America/New_York")
}

/// Counts runs; succeeds.
fn counter(n: &Arc<AtomicU32>) -> impl Fn() -> Effect<(), &'static str> + Send + Sync + 'static {
  let n = Arc::clone(n);
  move || {
    let n = Arc::clone(&n);
    sync(move || {
      n.fetch_add(1, Ordering::SeqCst);
    })
  }
}

/// Starts `registry` on a fresh executor whose wall clock reads `start`.
fn start(registry: &CronRegistry, start: &str) -> (TestExecutor, WallClock, CronScheduler) {
  let exec = TestExecutor::new();
  let clock = WallClock::anchored(exec.now(), utc(start));
  let _fiber = registry
    .run(clock.clone())
    .fork_root(Arc::new(exec.clone()));
  exec.run_until_stalled();
  (exec, clock, registry.scheduler())
}

fn scheduled_utc(runs: &[CronRun]) -> Vec<String> {
  runs
    .iter()
    .map(|r| r.scheduled.inner().timestamp().to_string())
    .collect()
}

fn outcomes(runs: &[CronRun]) -> Vec<CronOutcome> {
  runs.iter().map(|r| r.outcome.clone()).collect()
}

#[test]
fn spring_forward_gap_fires_once_shifted_by_the_gap() {
  let n = Arc::new(AtomicU32::new(0));
  let cron = Cron::parse("30 2 * * *")
    .expect("parse")
    .in_zone(new_york());
  let registry = CronRegistry::new()
    .with(CronJob::new("nightly", cron, counter(&n)))
    .expect("register");
  // 2026-03-07 07:00 EST; DST starts 2026-03-08 02:00 EST -> 03:00 EDT.
  let (exec, clock, sched) = start(&registry, "2026-03-07T12:00:00Z");
  exec.advance(3 * DAY);
  let runs = sched.history("nightly").expect("job");
  assert_eq!(
    scheduled_utc(&runs),
    [
      "2026-03-08T07:30:00Z", // 02:30 does not exist: 03:30 EDT
      "2026-03-09T06:30:00Z", // 02:30 EDT
      "2026-03-10T06:30:00Z",
    ]
  );
  assert_eq!(
    runs[0].scheduled.format_iso(),
    "2026-03-08T03:30:00-04:00[America/New_York]"
  );
  // Each run started at its fire time on the virtual wall clock.
  assert!(
    runs
      .iter()
      .all(|r| r.started == Some(UtcDateTime(r.scheduled.inner().timestamp())))
  );
  assert!(runs.iter().all(CronRun::is_success));
  assert_eq!(runs[0].duration(), Some(Duration::ZERO));
  assert_eq!(n.load(Ordering::SeqCst), 3);
  assert_eq!(clock.at(exec.now()), utc("2026-03-10T12:00:00Z"));
}

#[test]
fn hourly_job_keeps_real_hours_across_both_transitions() {
  let n = Arc::new(AtomicU32::new(0));
  let cron = Cron::parse("0 * * * *").expect("parse").in_zone(new_york());
  let registry = CronRegistry::new()
    .with(CronJob::new("hourly", cron, counter(&n)))
    .expect("register");
  // Spring forward: 01:00 EST, (02:00 -> 03:00 EDT), 04:00 EDT — one fire per real hour.
  let (exec, _, sched) = start(&registry, "2026-03-08T05:30:00Z");
  exec.advance(3 * HOUR);
  assert_eq!(
    scheduled_utc(&sched.history("hourly").expect("job")),
    [
      "2026-03-08T06:00:00Z",
      "2026-03-08T07:00:00Z",
      "2026-03-08T08:00:00Z"
    ]
  );

  // Fall back (2026-11-01 02:00 EDT -> 01:00 EST): civil 01:00 fires once, at its first
  // occurrence; the repeated hour does not fire again.
  let n = Arc::new(AtomicU32::new(0));
  let cron = Cron::parse("0 * * * *").expect("parse").in_zone(new_york());
  let registry = CronRegistry::new()
    .with(CronJob::new("hourly", cron, counter(&n)))
    .expect("register");
  let (exec, _, sched) = start(&registry, "2026-11-01T04:30:00Z");
  exec.advance(3 * HOUR);
  let runs = sched.history("hourly").expect("job");
  assert_eq!(
    scheduled_utc(&runs),
    ["2026-11-01T05:00:00Z", "2026-11-01T07:00:00Z"]
  );
  assert_eq!(
    runs[0].scheduled.format_iso(),
    "2026-11-01T01:00:00-04:00[America/New_York]"
  );
}

#[test]
fn fold_started_in_the_repeated_hour_uses_the_second_occurrence() {
  let cron = Cron::parse("30 1 * * *")
    .expect("parse")
    .in_zone(new_york());
  // 01:10 EST, inside the repeated hour: 01:30 EDT has passed, 01:30 EST has not.
  let next = cron.next_after(utc("2026-11-01T06:10:00Z")).expect("fires");
  assert_eq!(
    next.format_iso(),
    "2026-11-01T01:30:00-05:00[America/New_York]"
  );
  // From before the fold, the daily job fires once, at the first 01:30.
  let fires: Vec<String> = cron
    .upcoming(utc("2026-11-01T04:00:00Z"))
    .take(2)
    .map(|z| z.inner().timestamp().to_string())
    .collect();
  assert_eq!(fires, ["2026-11-01T05:30:00Z", "2026-11-02T06:30:00Z"]);
}

fn missed_case(policy: MissedRunPolicy) -> (Vec<String>, Vec<CronOutcome>, u32) {
  let n = Arc::new(AtomicU32::new(0));
  let registry = CronRegistry::new()
    .with(
      CronJob::new(
        "hourly",
        Cron::parse("@hourly").expect("parse"),
        counter(&n),
      )
      .missed_runs(policy),
    )
    .expect("register");
  let (exec, clock, sched) = start(&registry, "2026-01-01T00:30:00Z");
  exec.advance(Duration::from_secs(600));
  // The process is suspended for 3 h while the scheduler sleeps until 01:00: it wakes at
  // wall-clock 04:00 with 01:00, 02:00, 03:00 and 04:00 all due.
  clock.jump(3 * HOUR);
  exec.advance(Duration::from_secs(1200));
  let runs = sched.history("hourly").expect("job");
  (
    scheduled_utc(&runs),
    outcomes(&runs),
    n.load(Ordering::SeqCst),
  )
}

#[test]
fn missed_runs_are_skipped_by_default() {
  let (scheduled, outcomes, runs) = missed_case(MissedRunPolicy::Skip);
  assert_eq!(
    scheduled,
    [
      "2026-01-01T01:00:00Z",
      "2026-01-01T02:00:00Z",
      "2026-01-01T03:00:00Z",
      "2026-01-01T04:00:00Z"
    ]
  );
  let missed = CronOutcome::Skipped(CronSkip::Missed);
  assert_eq!(
    outcomes,
    [
      missed.clone(),
      missed.clone(),
      missed,
      CronOutcome::Succeeded
    ]
  );
  assert_eq!(runs, 1);
}

#[test]
fn missed_runs_catch_up_up_to_max() {
  let (scheduled, outcomes, runs) = missed_case(MissedRunPolicy::CatchUp { max: 2 });
  // 01:00 is beyond `max` and skipped; 02:00 and 03:00 catch up before 04:00.
  assert_eq!(
    scheduled,
    [
      "2026-01-01T01:00:00Z",
      "2026-01-01T02:00:00Z",
      "2026-01-01T03:00:00Z",
      "2026-01-01T04:00:00Z"
    ]
  );
  assert_eq!(
    outcomes,
    [
      CronOutcome::Skipped(CronSkip::Missed),
      CronOutcome::Succeeded,
      CronOutcome::Succeeded,
      CronOutcome::Succeeded
    ]
  );
  assert_eq!(runs, 3);
}

/// A job taking 90 s on a once-a-minute schedule.
fn slow_job(policy: OverlapPolicy, live: &Arc<AtomicU32>, peak: &Arc<AtomicU32>) -> CronJob {
  let (live, peak) = (Arc::clone(live), Arc::clone(peak));
  CronJob::new(
    "slow",
    Cron::parse("* * * * *").expect("parse"),
    move || {
      let (live, peak, done) = (Arc::clone(&live), Arc::clone(&peak), Arc::clone(&live));
      sync::<(), &str, _>(move || {
        let now = live.fetch_add(1, Ordering::SeqCst) + 1;
        peak.fetch_max(now, Ordering::SeqCst);
      })
      .then(sleep(Duration::from_secs(90)))
      .then(sync(move || {
        done.fetch_sub(1, Ordering::SeqCst);
      }))
    },
  )
  .overlap(policy)
}

#[test]
fn overlapping_fire_times_are_skipped_by_default() {
  let (live, peak) = (Arc::new(AtomicU32::new(0)), Arc::new(AtomicU32::new(0)));
  let registry = CronRegistry::new()
    .with(slow_job(OverlapPolicy::Skip, &live, &peak))
    .expect("register");
  let (exec, _, sched) = start(&registry, "2026-01-01T00:00:00Z");
  exec.advance(Duration::from_secs(4 * 60));
  // Records are appended when a fire time ends: 00:01 runs until 00:02:30, so 00:02 is
  // skipped first; 00:03 runs until 00:04:30, so 00:04 is skipped and 00:03 is in flight.
  let runs = sched.history("slow").expect("job");
  assert_eq!(
    scheduled_utc(&runs),
    [
      "2026-01-01T00:02:00Z",
      "2026-01-01T00:01:00Z",
      "2026-01-01T00:04:00Z"
    ]
  );
  let overlap = CronOutcome::Skipped(CronSkip::Overlap);
  assert_eq!(
    outcomes(&runs),
    [overlap.clone(), CronOutcome::Succeeded, overlap]
  );
  assert_eq!(peak.load(Ordering::SeqCst), 1);
  assert_eq!(sched.running("slow"), Some(1));
}

#[test]
fn overlap_allow_runs_concurrently() {
  let (live, peak) = (Arc::new(AtomicU32::new(0)), Arc::new(AtomicU32::new(0)));
  let registry = CronRegistry::new()
    .with(slow_job(OverlapPolicy::Allow, &live, &peak))
    .expect("register");
  let (exec, _, sched) = start(&registry, "2026-01-01T00:00:00Z");
  exec.advance(Duration::from_secs(4 * 60));
  let runs = sched.history("slow").expect("job");
  // Runs finish out of fire order: 00:01 ends at 00:02:30, 00:02 at 00:03:30.
  assert_eq!(runs.len(), 2);
  assert!(runs.iter().all(CronRun::is_success));
  assert_eq!(peak.load(Ordering::SeqCst), 2);
  assert_eq!(sched.running("slow"), Some(2));
}

#[test]
fn hooks_see_successes_and_failures_and_failures_do_not_stop_the_job() {
  let calls = Arc::new(AtomicU32::new(0));
  let c = Arc::clone(&calls);
  let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
  let (ok, bad) = (Arc::clone(&seen), Arc::clone(&seen));
  let job = CronJob::new(
    "flaky",
    Cron::parse("@hourly").expect("parse"),
    move || match c.fetch_add(1, Ordering::SeqCst) {
      0 => fail::<(), &str, _>("db down"),
      1 => sync(|| panic!("bug")),
      _ => succeed(()),
    },
  )
  .on_success(move |run: &CronRun| {
    let (seen, at) = (Arc::clone(&ok), run.scheduled.inner().hour());
    sync(move || seen.lock().expect("lock").push(format!("ok@{at}")))
  })
  .on_failure(move |run: &CronRun| {
    let (seen, outcome) = (Arc::clone(&bad), run.outcome.clone());
    sync(move || seen.lock().expect("lock").push(format!("{outcome:?}")))
  })
  // A hook that dies is ignored.
  .on_failure(|_: &CronRun| sync(|| panic!("hook bug")));
  let registry = CronRegistry::new().with(job).expect("register");
  let (exec, _, sched) = start(&registry, "2026-01-01T00:00:00Z");
  exec.advance(3 * HOUR);
  let seen = seen.lock().expect("lock").clone();
  assert_eq!(seen[0], r#"Failed("db down")"#);
  assert!(seen[1].starts_with(r#"Failed("defect: "#), "{}", seen[1]);
  assert_eq!(seen[2], "ok@3");
  assert_eq!(calls.load(Ordering::SeqCst), 3);
  let last = sched.last_run("flaky").expect("ran");
  assert!(last.is_success());
  assert_eq!(&*last.job, "flaky");
}

#[test]
fn history_is_bounded() {
  let n = Arc::new(AtomicU32::new(0));
  let registry = CronRegistry::new()
    .with(
      CronJob::new(
        "tick",
        Cron::parse("* * * * *").expect("parse"),
        counter(&n),
      )
      .history_limit(2),
    )
    .expect("register")
    .with(
      CronJob::new(
        "quiet",
        Cron::parse("* * * * *").expect("parse"),
        counter(&n),
      )
      .history_limit(0),
    )
    .expect("register");
  let (exec, _, sched) = start(&registry, "2026-01-01T00:00:00Z");
  exec.advance(Duration::from_secs(5 * 60));
  assert_eq!(n.load(Ordering::SeqCst), 10);
  assert_eq!(
    scheduled_utc(&sched.history("tick").expect("job")),
    ["2026-01-01T00:04:00Z", "2026-01-01T00:05:00Z"]
  );
  assert!(sched.history("quiet").expect("job").is_empty());
  assert!(sched.history("nope").is_none());
  assert!(sched.running("nope").is_none());
}

#[test]
fn registry_is_declarative_and_rejects_bad_entries() {
  let n = Arc::new(AtomicU32::new(0));
  let registry = CronRegistry::new()
    .schedule("a", "0 3 * * *", counter(&n))
    .and_then(|r| r.schedule("b", "*/5 * * * *", counter(&n)))
    .expect("register");
  assert_eq!(registry.len(), 2);
  assert!(!registry.is_empty());
  assert_eq!(
    registry
      .scheduler()
      .job_names()
      .iter()
      .map(|s| s.to_string())
      .collect::<Vec<_>>(),
    ["a", "b"]
  );
  let dup = registry.clone().schedule("a", "@daily", counter(&n));
  assert_eq!(dup.unwrap_err(), CronError::DuplicateJob("a".into()));
  let bad = CronRegistry::new().schedule("c", "61 * * * *", counter(&n));
  let Err(CronError::Parse(e)) = bad else {
    panic!("expected a parse error")
  };
  assert_eq!(e.expression, "61 * * * *");
  assert!(format!("{registry:?}").contains("\"a\""));
  assert!(CronRegistry::<HNil>::new().is_empty());
}

#[test]
fn an_expression_that_never_fires_ends_its_loop() {
  let n = Arc::new(AtomicU32::new(0));
  let registry = CronRegistry::new()
    .schedule("never", "0 0 30 2 *", counter(&n))
    .expect("register");
  let exec = TestExecutor::new();
  let clock = WallClock::anchored(exec.now(), utc("2026-01-01T00:00:00Z"));
  let fiber = registry.run(clock).fork_root(Arc::new(exec.clone()));
  exec.run_until_stalled();
  assert!(fiber.is_done());
}

#[test]
fn layer_provides_the_scheduler_and_stops_jobs_with_its_scope() {
  let n = Arc::new(AtomicU32::new(0));
  let registry = CronRegistry::new()
    .schedule("tick", "* * * * *", counter(&n))
    .expect("register");
  let exec = TestExecutor::new();
  let clock = WallClock::anchored(exec.now(), utc("2026-01-01T00:00:00Z"));
  let wait = sleep::<Never, HCons<CronScheduler, HNil>>(Duration::from_secs(150));
  let program: Effect<usize> = wait
    .then(need::<CronScheduler>().map(|s| s.history("tick").expect("job").len()))
    .provide_layer(registry.layer(clock));
  let fiber = program.fork_root(Arc::new(exec.clone()));
  exec.advance(Duration::from_secs(150));
  assert!(fiber.is_done());
  assert_eq!(n.load(Ordering::SeqCst), 2);
  // The scope closed with the program: no more runs.
  exec.advance(Duration::from_secs(600));
  assert_eq!(n.load(Ordering::SeqCst), 2);
}

#[test]
fn schedule_cron_drives_repeat_on_virtual_time() {
  let exec = TestExecutor::new();
  let clock = WallClock::anchored(exec.now(), utc("2026-01-01T00:00:10Z"));
  let n = Arc::new(AtomicU32::new(0));
  let run = counter(&n);
  let schedule = Schedule::cron(Cron::parse("*/20 * * * * *").expect("parse"), clock.clone())
    .intersect(Schedule::recurs(2));
  let eff: Effect<(Option<ZonedDateTime>, u64), &str> = Effect::repeat(run, schedule);
  let start = exec.now();
  let fiber = eff.fork_root(Arc::new(exec.clone()));
  exec.advance(Duration::from_secs(60));
  assert!(fiber.is_done());
  // Runs at :10 (immediately), :20 and :40.
  assert_eq!(n.load(Ordering::SeqCst), 3);
  assert_eq!(
    clock.at(start + Duration::from_secs(30)),
    utc("2026-01-01T00:00:40Z")
  );
  let never =
    Schedule::<Option<ZonedDateTime>, ()>::cron(Cron::parse("0 0 30 2 *").expect("parse"), clock);
  let step = never.step(exec.now(), &(), never.start());
  assert!(step.is_done() && step.out().is_none());
}
