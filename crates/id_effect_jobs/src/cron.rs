//! Cron schedules that enqueue jobs (feature `cron`): [`enqueue_on_tick`] turns a core
//! [`CronJob`] into a producer, and [`CronLeader`] runs a [`CronRegistry`] on one node at a time.
//!
//! - Each fire time enqueues one job keyed [`tick_key`] = `{name}@{tick}` (the tick as an
//!   RFC 3339 UTC instant) with [`UniqueScope::UntilPruned`](crate::UniqueScope::UntilPruned):
//!   however many schedulers run the same entry, and even when one enqueues after the first
//!   tick's job already finished, the tick is inserted once (until the row is pruned).
//! - [`CronLeader`] elects the scheduler with a [`ClusterSingleton`] (production: over a
//!   [`PgAdvisoryLease`](id_effect_node::singleton::PgAdvisoryLease)): the holder runs the
//!   registry and heartbeats the lease; once a heartbeat fails it stops its schedule, resigns and
//!   competes again after a retry interval, and a follower that acquires the lease takes the
//!   schedule over. A tick both enqueue around a handover is still inserted once, thanks to the
//!   key.
//!
//! The job then runs on any [`Worker`](crate::Worker), with the queue's retries; the cron entry
//! only records whether the enqueue worked. The example is on [`CronLeader`].

use std::sync::Arc;
use std::time::Duration;

use id_effect::{
  Cron, CronJob, CronRegistry, Effect, Exit, HList, HNil, Never, Selector, UtcDateTime, WallClock,
  ZonedDateTime, sleep, succeed,
};
use id_effect_node::singleton::ClusterSingleton;
use id_effect_process::Pid;

use crate::job::{Job, UniqueKey};
use crate::queue::{JobOptions, JobQueueService, RunAt, enqueue};

/// The [`UniqueKey`] of the job the cron entry `name` enqueues for `tick`: `{name}@{tick}` (the
/// tick as an RFC 3339 UTC instant), held until the row is pruned.
pub fn tick_key(name: &str, tick: &ZonedDateTime) -> UniqueKey {
  UniqueKey::until_pruned(format!("{name}@{}", tick.inner().timestamp()))
}

/// A cron entry named `name` that enqueues `make(tick)` on the [`JobQueueService`] in `R` at every
/// fire time of `cron`, keyed [`tick_key`]`(name, tick)` and runnable at the tick. A duplicate
/// (another scheduler enqueued the tick) is a success; a queue error is the run's failure.
pub fn enqueue_on_tick<J, F, R, I>(name: impl Into<Arc<str>>, cron: Cron, make: F) -> CronJob<R>
where
  J: Job,
  F: Fn(&ZonedDateTime) -> J + Send + Sync + 'static,
  R: Selector<JobQueueService, I> + HList + Send + Sync + 'static,
  I: 'static,
{
  let name: Arc<str> = name.into();
  let key_name = Arc::clone(&name);
  CronJob::new_at(name, cron, move |tick: ZonedDateTime| {
    let opts = JobOptions::new()
      .unique(tick_key(&key_name, &tick))
      .run_at(RunAt::At(UtcDateTime(tick.inner().timestamp())));
    enqueue::<J, R, I>(make(&tick), opts)
  })
}

/// Runs a [`CronRegistry`] only while this node holds a [`ClusterSingleton`] (production: over a
/// [`PgAdvisoryLease`](id_effect_node::singleton::PgAdvisoryLease)). The holder runs the registry
/// and heartbeats the lease; once a heartbeat fails it stops its schedule, resigns and competes
/// again after a retry interval, and a follower that acquires the lease takes the schedule over.
/// Entries built with [`enqueue_on_tick`] insert a tick both nodes enqueue around a handover
/// once. Cloning shares the singleton.
///
/// ```
/// use std::sync::Arc;
/// use std::time::Duration;
///
/// use id_effect::{
///   Cron, CronRegistry, Executor, Exit, Schema, TestExecutor, UtcDateTime, WallClock,
/// };
/// use id_effect_jobs::{
///   CronLeader, Job, JobFilter, JobQueue, JobQueueEnv, MemoryJobQueue, enqueue_on_tick,
///   job_queue_env,
/// };
/// use id_effect_node::singleton::{ClusterSingleton, MemoryLease};
/// use id_effect_process::{NodeName, Pid};
///
/// #[derive(Clone, Schema)]
/// struct Report {
///   kind: String,
/// }
///
/// impl Job for Report {
///   const NAME: &'static str = "app.report";
/// }
///
/// let cron = Cron::parse("*/15 * * * *").unwrap();
/// let registry = CronRegistry::<JobQueueEnv>::new()
///   .with(enqueue_on_tick("report", cron, |_tick| Report { kind: "hourly".into() }))
///   .unwrap();
///
/// let exec = TestExecutor::new();
/// let start = UtcDateTime::from_epoch_millis(1_767_225_600_000).unwrap(); // 2026-01-01T00:00Z
/// let clock = WallClock::anchored(exec.now(), start);
/// let store = MemoryJobQueue::new(clock.clone());
/// let lease = Arc::new(MemoryLease::new());
/// let singleton = ClusterSingleton::new("cron", "node-a", lease, Duration::from_secs(3600));
/// let leader = CronLeader::new(singleton, Pid::from_parts(NodeName::new("a@host"), 1, 1));
/// let fiber = leader
///   .run(&registry, clock)
///   .provide_all(job_queue_env(store.clone()))
///   .fork_root(Arc::new(exec.clone()));
/// exec.advance(Duration::from_secs(3600)); // 00:15, 00:30, 00:45, 01:00
/// let Exit::Success(jobs) = store.list(JobFilter::default()).block_on() else {
///   panic!("list");
/// };
/// let keys: Vec<_> = jobs
///   .into_iter()
///   .map(|job| job.unique.unwrap().key)
///   .collect();
/// assert_eq!(keys[0], "report@2026-01-01T01:00:00Z"); // newest first
/// assert_eq!(keys.len(), 4);
/// fiber.interrupt_now();
/// ```
#[derive(Clone)]
pub struct CronLeader {
  singleton: Arc<ClusterSingleton>,
  pid: Pid,
  retry_every: Duration,
  heartbeat_every: Duration,
}

impl CronLeader {
  /// Leads with `singleton`, recording `pid` as the holder's local process. A follower retries
  /// the lease and the leader heartbeats it every 5 s by default; a TTL-based lease needs a TTL
  /// well above [`heartbeat_every`](Self::heartbeat_every).
  pub fn new(singleton: ClusterSingleton, pid: Pid) -> Self {
    Self {
      singleton: Arc::new(singleton),
      pid,
      retry_every: Duration::from_secs(5),
      heartbeat_every: Duration::from_secs(5),
    }
  }

  /// How often a follower tries to acquire the lease.
  pub fn retry_every(mut self, every: Duration) -> Self {
    self.retry_every = every;
    self
  }

  /// How often the leader renews the lease; a failed renewal ends its schedule.
  pub fn heartbeat_every(mut self, every: Duration) -> Self {
    self.heartbeat_every = every;
    self
  }

  /// The singleton this leader competes for.
  pub fn singleton(&self) -> &ClusterSingleton {
    &self.singleton
  }

  /// Competes for the lease and runs `registry` on `clock` while holding it, until interrupted
  /// (the schedule stops and the lease is released) or, as leader, no job will fire again. The
  /// schedule starts from the instant the lease is won: ticks before it are not due.
  pub fn run<R>(&self, registry: &CronRegistry<R>, clock: WallClock) -> Effect<(), Never, R>
  where
    R: HList + Send + Sync + 'static,
  {
    follow(self.clone(), registry.clone(), clock)
  }
}

impl std::fmt::Debug for CronLeader {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("CronLeader")
      .field("pid", &self.pid)
      .field("retry_every", &self.retry_every)
      .field("heartbeat_every", &self.heartbeat_every)
      .finish_non_exhaustive()
  }
}

/// A follower: try the lease, lead on success, else wait and try again.
fn follow<R>(
  leader: CronLeader,
  registry: CronRegistry<R>,
  clock: WallClock,
) -> Effect<(), Never, R>
where
  R: HList + Send + Sync + 'static,
{
  leader
    .singleton
    .try_become(&leader.pid)
    .exit::<Never>()
    .narrow::<R, HNil>()
    .flat_map(move |won| match won {
      Exit::Success(()) => lead(&leader, &registry, &clock).flat_map(move |finished| {
        if finished {
          succeed(())
        } else {
          // Lost: give the other nodes a retry interval to take over before competing again.
          sleep(leader.retry_every).then(follow(leader, registry, clock))
        }
      }),
      Exit::Failure(_) => sleep(leader.retry_every).then(follow(leader, registry, clock)),
    })
}

/// The leader: the schedule raced against the heartbeat, then resign. `true` when the schedule
/// ended by itself (nothing fires again), `false` when the lease was lost.
fn lead<R>(
  leader: &CronLeader,
  registry: &CronRegistry<R>,
  clock: &WallClock,
) -> Effect<bool, Never, R>
where
  R: HList + Send + Sync + 'static,
{
  let schedule = registry.run(clock.clone()).map(|()| true);
  let lost = watch(Arc::clone(&leader.singleton), leader.heartbeat_every)
    .narrow::<R, HNil>()
    .map(|()| false);
  let resign = leader
    .singleton
    .resign()
    .exit::<Never>()
    .map(|_| ())
    .narrow::<R, HNil>();
  schedule.race_first(lost).ensuring(resign)
}

/// Renews the lease every `every`; completes at the first failed renewal.
fn watch(singleton: Arc<ClusterSingleton>, every: Duration) -> Effect<(), Never> {
  sleep(every).flat_map(move |()| {
    singleton
      .heartbeat()
      .exit::<Never>()
      .flat_map(move |renewed| match renewed {
        Exit::Success(()) => watch(singleton, every),
        Exit::Failure(_) => succeed(()),
      })
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::{
    Enqueued, JobFilter, JobQueue, JobQueueEnv, JobQueueExt, JobRecord, MemoryJobQueue,
    job_queue_env,
  };
  use id_effect::{Executor, Schema, TestExecutor};
  use id_effect_node::singleton::{MemoryLease, SingletonLease};
  use id_effect_process::NodeName;

  const MINUTE: Duration = Duration::from_secs(60);
  const HOUR: Duration = Duration::from_secs(3600);

  #[derive(Clone, Schema)]
  struct Report {
    by: String,
  }

  impl Job for Report {
    const NAME: &'static str = "cron.report";
  }

  fn registry(by: &'static str) -> CronRegistry<JobQueueEnv> {
    let cron = Cron::parse("*/15 * * * *").unwrap();
    CronRegistry::new()
      .with(enqueue_on_tick("report", cron, move |_| Report {
        by: by.into(),
      }))
      .unwrap()
  }

  fn setup() -> (TestExecutor, WallClock, MemoryJobQueue) {
    let exec = TestExecutor::new();
    let start = UtcDateTime::from_epoch_millis(1_767_225_600_000).unwrap(); // 2026-01-01T00:00Z
    let clock = WallClock::anchored(exec.now(), start);
    let store = MemoryJobQueue::new(clock.clone());
    (exec, clock, store)
  }

  /// Runs a store or lease effect (they never sleep) to its value.
  fn now<A: Send + 'static, E: std::fmt::Debug + Send + 'static>(eff: Effect<A, E>) -> A {
    match eff.block_on() {
      Exit::Success(a) => a,
      Exit::Failure(c) => panic!("failed: {c:?}"),
    }
  }

  /// Every job, oldest first, as `(unique key, by)`.
  fn jobs(store: &MemoryJobQueue) -> Vec<(String, String)> {
    let mut all: Vec<JobRecord> = now(store.list(JobFilter::default()));
    all.reverse();
    all
      .into_iter()
      .map(|j| {
        (
          j.unique.unwrap().key,
          j.payload["by"].as_str().unwrap().to_owned(),
        )
      })
      .collect()
  }

  fn pid() -> Pid {
    Pid::from_parts(NodeName::new("a@t"), 1, 1)
  }

  #[test]
  fn cron_tick_key_is_name_at_utc_instant() {
    let cron = Cron::parse("30 2 * * *").unwrap();
    let start = UtcDateTime::from_epoch_millis(1_767_225_600_000).unwrap();
    let tick = cron.next_after(start).unwrap();
    let key = tick_key("nightly", &tick);
    assert_eq!(key.key, "nightly@2026-01-01T02:30:00Z");
    assert_eq!(key.scope, crate::UniqueScope::UntilPruned);
  }

  #[test]
  fn cron_two_schedulers_enqueue_each_tick_once_even_after_it_ran() {
    let (exec, clock, store) = setup();
    let env = job_queue_env(store.clone());
    let a = registry("a").run(clock.clone()).provide_all(env.clone());
    let b = registry("b").run(clock).provide_all(env);
    let fa = a.fork_root(Arc::new(exec.clone()));
    let fb = b.fork_root(Arc::new(exec.clone()));
    exec.advance(HOUR);
    let keys: Vec<_> = jobs(&store).into_iter().map(|(k, _)| k).collect();
    assert_eq!(
      keys,
      [
        "report@2026-01-01T00:15:00Z",
        "report@2026-01-01T00:30:00Z",
        "report@2026-01-01T00:45:00Z",
        "report@2026-01-01T01:00:00Z",
      ]
    );
    // The key outlives the job: a late scheduler re-enqueuing a finished tick inserts nothing.
    let id = now(store.list(JobFilter::default())).last().unwrap().id;
    assert!(now(store.cancel(id)), "the first tick's job is finished");
    let tick = Cron::parse("*/15 * * * *")
      .unwrap()
      .next_after(UtcDateTime::from_epoch_millis(1_767_225_600_000).unwrap())
      .unwrap();
    let again = store.enqueue(
      Report { by: "late".into() },
      JobOptions::new().unique(tick_key("report", &tick)),
    );
    assert_eq!(now(again), Enqueued::Duplicate { existing: Some(id) });
    fa.interrupt_now();
    fb.interrupt_now();
  }

  #[test]
  fn cron_leader_hands_over_when_its_lease_is_lost() {
    let (exec, clock, store) = setup();
    let lease = Arc::new(MemoryLease::new());
    let leader = |holder: &'static str| {
      let single = ClusterSingleton::new("cron", holder, Arc::clone(&lease) as _, HOUR);
      CronLeader::new(single, pid())
        .retry_every(MINUTE)
        .heartbeat_every(MINUTE)
    };
    let (a, b) = (leader("a"), leader("b"));
    let env = job_queue_env(store.clone());
    let fa = a
      .run(&registry("a"), clock.clone())
      .provide_all(env.clone())
      .fork_root(Arc::new(exec.clone()));
    exec.run_until_stalled();
    let fb = b
      .run(&registry("b"), clock)
      .provide_all(env)
      .fork_root(Arc::new(exec.clone()));
    exec.advance(30 * MINUTE);
    let holds = |l: &CronLeader| now(l.singleton().is_holder());
    assert!(holds(&a) && !holds(&b));
    assert_eq!(a.singleton().local_pid(), Some(pid()));
    // The lease expires under `a` (e.g. a partition): its next heartbeat fails.
    now(lease.release("cron", "a"));
    exec.advance(30 * MINUTE);
    assert!(!holds(&a) && holds(&b), "b took over");
    assert_eq!(b.singleton().local_pid(), Some(pid()));
    let by: Vec<_> = jobs(&store).into_iter().map(|(_, by)| by).collect();
    assert_eq!(by, ["a", "a", "b", "b"]);
    fa.interrupt_now();
    fb.interrupt_now();
  }
}
