# Cron Jobs — Calendar Schedules

`Schedule::spaced` answers "every ten minutes from now". Background work in a real service is
usually tied to the calendar instead: "at 02:30 New York time", "every six hours on the hour",
"on the first of the month". This section covers cron expressions, how they behave across
daylight-saving changes, and how to run a set of named jobs with overlap and missed-run policies,
hooks and run history.

## Cron Expressions

`Cron::parse` accepts the classic five fields — minute, hour, day of month, month, day of week —
or six with a leading second. Fields take `*`, values, ranges `a-b`, lists `a,b` and steps `*/n`;
months and weekdays also take names (`JAN`, `mon`). The usual macros (`@hourly`, `@daily`,
`@weekly`, `@monthly`, `@yearly`) work too. A bad expression is a typed `CronParseError`:

```rust
use id_effect::{Cron, UtcDateTime};

let cron = Cron::parse("0 9-17/4 * * MON-FRI").unwrap();
let from = UtcDateTime("2026-01-01T10:00:00Z".parse().unwrap()); // a Thursday
let next = cron.next_after(from).unwrap();
assert_eq!(next.format_iso(), "2026-01-01T13:00:00+00:00[UTC]");

assert!(Cron::parse("61 * * * *").is_err());
```

When both day fields are restricted (`0 0 15 * MON`) a day matches if **either** does, as in
classic cron; if either is `*`, both must.

## Time Zones and Daylight Saving

An expression is evaluated in UTC unless you give it a zone with `in_zone`. It is then matched
against **wall-clock time in that zone**, and each matching wall-clock time fires at most once:

- When clocks spring forward, a time that does not exist (02:30 on the transition night) fires
  shifted forward by the gap — at 03:30.
- When clocks fall back, a time that happens twice (01:30) fires at its first occurrence only, so
  a nightly job does not run twice.

```rust
use id_effect::{Cron, UtcDateTime, timezone};

let new_york = timezone::from_str("America/New_York").unwrap();
let nightly = Cron::parse("30 2 * * *").unwrap().in_zone(new_york);
let fires: Vec<String> = nightly
    .upcoming(UtcDateTime("2026-03-07T12:00:00Z".parse().unwrap()))
    .take(2)
    .map(|t| t.format_iso())
    .collect();
assert_eq!(fires, [
    "2026-03-08T03:30:00-04:00[America/New_York]", // 02:30 does not exist that night
    "2026-03-09T02:30:00-04:00[America/New_York]",
]);
```

## The Wall Clock

The runtime's timers run on the executor's monotonic clock — virtual time on a `TestExecutor`.
Cron needs the calendar, so a `WallClock` maps one to the other:

- `WallClock::system()` reads the system's real-time clock. Use it in production.
- `WallClock::anchored(instant, at)` says "executor instant `instant` is date-time `at`" and
  advances with the executor clock. Under a `TestExecutor`, `advance` moves the calendar too.
- `jump(d)` moves an anchored clock forward without moving the executor clock — what a scheduler
  sees when a laptop wakes from sleep or NTP steps the clock.

For a one-off recurring effect, `Schedule::cron(cron, clock)` is an ordinary `Schedule`: it
sleeps until the next fire time and works with `Effect::repeat` like any other schedule.

## Named Jobs and the Registry

Most services want a single declarative list of their jobs. A `CronRegistry` holds named
`CronJob`s; each job is an expression plus a factory for the effect to run (an effect runs once,
so the job takes a `Fn() -> Effect`). Names are unique — registering one twice is a typed
`CronError::DuplicateJob`:

```rust
# use id_effect::{Cron, CronError, CronJob, CronRegistry, Effect, succeed, timezone};
# fn report() -> Effect<(), String> { succeed(()) }
# fn sync_upstream() -> Effect<(), String> { succeed(()) }
# fn ping() -> Effect<(), String> { succeed(()) }
# let new_york = timezone::from_str("America/New_York").unwrap();
let registry = CronRegistry::new()
    .with(CronJob::new("nightly-report", Cron::parse("30 2 * * *")?.in_zone(new_york), report))?
    .schedule("sync", "0 */6 * * *", sync_upstream)?
    .schedule("heartbeat", "@hourly", ping)?;
# assert_eq!(registry.len(), 3);
# Ok::<(), CronError>(())
```

Run the jobs with `registry.run(clock)` — an effect that runs every job's scheduler fiber until
it is interrupted — or provide them for the lifetime of a layer with `registry.layer(clock)`,
which starts the jobs when the layer is built, provides a `CronScheduler` capability for reading
their history, and stops them (interrupting runs in flight) when the layer's scope closes.

Each job's scheduler is a fiber that sleeps until the next fire time. The run itself is forked as
a child fiber, so a slow job never delays the schedule and stopping the scheduler interrupts its
runs cleanly. A failed run — typed error or panic — is recorded and reported, and the job keeps
its schedule.

## Overlap and Missed Runs

Two policies decide what happens when reality does not match the calendar:

| Policy | Situation | Options |
|--------|-----------|---------|
| `OverlapPolicy` | A fire time arrives while the previous run is still going | `Skip` (default) records it as skipped; `Allow` starts a concurrent run |
| `MissedRunPolicy` | The scheduler wakes up after several fire times passed (suspended host, clock step) | `Skip` (default) runs only the latest; `CatchUp { max }` runs up to `max` of the missed ones first, oldest first |

```rust
# use id_effect::{Cron, CronError, CronJob, Effect, MissedRunPolicy, OverlapPolicy, succeed};
# fn sync_upstream() -> Effect<(), String> { succeed(()) }
# let job: CronJob =
CronJob::new("sync", Cron::parse("0 */6 * * *")?, sync_upstream)
    .overlap(OverlapPolicy::Skip)
    .missed_runs(MissedRunPolicy::CatchUp { max: 3 })
# ;
# assert_eq!(job.name(), "sync");
# Ok::<(), CronError>(())
```

## Monitoring: Hooks and History

`on_success` and `on_failure` add hooks — effects in the job's environment that receive the
`CronRun` record — for alerting or heartbeat pings. Every run and every skipped fire time is
kept in a bounded history (the last 100 by default, `history_limit` to change), available from
`CronScheduler::history` and `last_run`:

```rust
# use id_effect::{Cron, CronError, CronJob, CronRegistry, CronRun, Effect, Never, succeed, sync};
# fn sync_upstream() -> Effect<(), String> { succeed(()) }
# fn alert(message: String) -> Effect<(), Never> { sync(move || eprintln!("{message}")) }
let job = CronJob::new("sync", Cron::parse("0 */6 * * *")?, sync_upstream)
    .on_failure(|run: &CronRun| alert(format!("{} failed: {:?}", run.job, run.outcome)));
// ...
# let registry = CronRegistry::new().with(job)?;
# let scheduler = registry.scheduler();
for run in scheduler.history("sync").unwrap() {
    println!("{} {:?}", run.scheduled.format_iso(), run.outcome);
}
# Ok::<(), CronError>(())
```

## Testing Jobs on Virtual Time

Because the scheduler sleeps on the executor clock, a `TestExecutor` with an anchored wall clock
runs days of schedule instantly and deterministically — including a DST transition or a
simulated suspend:

```rust
use std::sync::Arc;
use std::time::Duration;
use id_effect::{
    CronRegistry, CronOutcome, CronSkip, Executor, TestExecutor, UtcDateTime, WallClock, succeed,
};

let registry = CronRegistry::new()
    .schedule("hourly", "@hourly", || succeed::<(), &str, _>(()))
    .unwrap();
let exec = TestExecutor::new();
let clock = WallClock::anchored(exec.now(), UtcDateTime("2026-01-01T00:30:00Z".parse().unwrap()));
let scheduler = registry.scheduler();
let fiber = registry.run(clock.clone()).fork_root(Arc::new(exec.clone()));

exec.advance(Duration::from_secs(600));
clock.jump(Duration::from_secs(3 * 3600)); // the host sleeps for three hours
exec.advance(Duration::from_secs(1200));   // wakes at 04:00: 01:00–03:00 were missed

let outcomes: Vec<CronOutcome> = scheduler
    .history("hourly")
    .unwrap()
    .into_iter()
    .map(|r| r.outcome)
    .collect();
assert_eq!(outcomes.len(), 4);
assert_eq!(outcomes[0], CronOutcome::Skipped(CronSkip::Missed));
assert_eq!(outcomes[3], CronOutcome::Succeeded);
fiber.interrupt_now();
```

`examples/123_cron_jobs.rs` puts it together: a registry with a zoned nightly job across the
spring-forward night, a catch-up policy, a failure hook and the resulting history.

Schedules are defined in code: adding or removing jobs at runtime without a redeploy, and
running each job once across a cluster, are not covered by the registry.
