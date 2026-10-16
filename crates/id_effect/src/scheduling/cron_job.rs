//! Named cron jobs: [`CronJob`] (expression, effect, overlap and missed-run policies, success /
//! failure hooks), the central [`CronRegistry`] that declares them, and the [`CronScheduler`]
//! handle with each job's run history.
//!
//! # Model
//!
//! Each job runs as one scheduler fiber (a loop of `sleep`s on the executor clock, so virtual
//! time on a [`TestExecutor`](crate::TestExecutor)). At every wake-up it reads the
//! [`WallClock`], collects the fire times due since the last one it handled, and dispatches them:
//!
//! - the run itself is forked as a child fiber of the scheduler fiber, so a slow job never
//!   delays the schedule, and stopping the scheduler (interrupting [`CronRegistry::run`], or
//!   closing the scope of [`CronRegistry::layer`]) interrupts the runs in flight;
//! - [`OverlapPolicy`] decides what happens when a fire time arrives while an earlier run of the
//!   same job is still going: `Skip` it (recorded as [`CronSkip::Overlap`]) or `Allow` a
//!   concurrent run;
//! - [`MissedRunPolicy`] decides what happens to fire times the scheduler woke up too late for
//!   (a suspended process, a clock step, a starved executor): the latest due fire time always
//!   runs; with `Skip` the older ones are recorded as [`CronSkip::Missed`], with
//!   `CatchUp { max }` up to `max` of them run first, oldest first, in one sequential batch.
//!
//! A run's typed failure or defect is recorded in the job's history ([`CronOutcome::Failed`]) and
//! passed to its `on_failure` hooks; it never stops the scheduler. Hooks are effects in the
//! job's environment, run after the run is recorded.
//!
//! ```
//! use std::sync::Arc;
//! use std::sync::atomic::{AtomicU32, Ordering};
//! use std::time::Duration;
//!
//! use id_effect::{CronRegistry, Executor, TestExecutor, UtcDateTime, WallClock, sync};
//!
//! let runs = Arc::new(AtomicU32::new(0));
//! let r = Arc::clone(&runs);
//! let registry = CronRegistry::new()
//!   .schedule("tick", "*/15 * * * *", move || {
//!     let r = Arc::clone(&r);
//!     sync::<(), &str, _>(move || { r.fetch_add(1, Ordering::SeqCst); })
//!   })
//!   .unwrap();
//!
//! let exec = TestExecutor::new();
//! let start = UtcDateTime::from_epoch_millis(1_767_225_600_000).unwrap(); // 2026-01-01T00:00Z
//! let clock = WallClock::anchored(exec.now(), start);
//! let scheduler = registry.scheduler();
//! let fiber = registry.run(clock).fork_root(Arc::new(exec.clone()));
//! exec.advance(Duration::from_secs(3600)); // virtual time: 00:15, 00:30, 00:45, 01:00
//! assert_eq!(runs.load(Ordering::SeqCst), 4);
//! assert_eq!(scheduler.history("tick").unwrap().len(), 4);
//! fiber.interrupt_now();
//! ```

use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use super::cron::{Cron, CronParseError, WallClock, delay_until};
use super::datetime::{UtcDateTime, ZonedDateTime};
use crate::capability::{HCons, HList, HNil};
use crate::concurrency::Concurrency;
use crate::failure::{Cause, Exit, Never};
use crate::kernel::{Effect, sleep, succeed, suspend, sync};
use crate::layer::Layer;
use crate::par::all;
use crate::scope::Scope;

/// The default number of runs kept per job by [`CronJob::history_limit`].
const DEFAULT_HISTORY: usize = 100;

// ── policies and records ────────────────────────────────────────────────────────────────────

/// What a fire time does while an earlier run of the same job is still in flight.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum OverlapPolicy {
  /// Skip it (recorded as [`CronSkip::Overlap`]). The default.
  #[default]
  Skip,
  /// Start another run concurrently.
  Allow,
}

/// What happens to fire times the scheduler woke up too late for (see the
/// [module docs](self)). The latest due fire time always runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum MissedRunPolicy {
  /// Skip the older ones (each recorded as [`CronSkip::Missed`]). The default.
  #[default]
  Skip,
  /// Run up to `max` of the older ones (the most recent `max`), oldest first, before the latest;
  /// skip the rest.
  CatchUp {
    /// The most missed fire times to run.
    max: usize,
  },
}

/// Why a fire time did not run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CronSkip {
  /// An earlier run was still in flight ([`OverlapPolicy::Skip`]).
  Overlap,
  /// The scheduler woke up after a later fire time was also due ([`MissedRunPolicy`]).
  Missed,
}

/// How one fire time of a job ended.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum CronOutcome {
  /// The run succeeded.
  Succeeded,
  /// The run failed: its typed error (`Display`), `defect: ..`, or `interrupted`.
  Failed(String),
  /// The fire time did not run.
  Skipped(CronSkip),
}

/// One entry of a job's run history.
#[derive(Clone, Debug)]
pub struct CronRun {
  /// The job's name.
  pub job: Arc<str>,
  /// The fire time, in the expression's zone.
  pub scheduled: ZonedDateTime,
  /// When the run started on the scheduler's wall clock (`None` when skipped).
  pub started: Option<UtcDateTime>,
  /// When the run ended on the scheduler's wall clock (`None` when skipped).
  pub finished: Option<UtcDateTime>,
  /// How it ended.
  pub outcome: CronOutcome,
}

impl CronRun {
  /// `true` for [`CronOutcome::Succeeded`].
  pub fn is_success(&self) -> bool {
    self.outcome == CronOutcome::Succeeded
  }

  /// Wall-clock time between start and end (`None` when skipped).
  pub fn duration(&self) -> Option<Duration> {
    Some(self.started?.distance_duration(&self.finished?))
  }

  fn skipped(job: &Arc<str>, scheduled: ZonedDateTime, why: CronSkip) -> Self {
    Self {
      job: Arc::clone(job),
      scheduled,
      started: None,
      finished: None,
      outcome: CronOutcome::Skipped(why),
    }
  }
}

/// A rejected [`CronRegistry`] registration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CronError {
  /// The expression did not parse.
  Parse(CronParseError),
  /// A job with this name is already registered.
  DuplicateJob(String),
}

impl fmt::Display for CronError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Parse(e) => e.fmt(f),
      Self::DuplicateJob(name) => write!(f, "cron job {name:?} is already registered"),
    }
  }
}

impl std::error::Error for CronError {}

impl From<CronParseError> for CronError {
  fn from(e: CronParseError) -> Self {
    Self::Parse(e)
  }
}

// ── jobs ────────────────────────────────────────────────────────────────────────────────────

type Task<R> = Arc<dyn Fn() -> Effect<(), String, R> + Send + Sync>;
type Hook<R> = Arc<dyn Fn(&CronRun) -> Effect<(), Never, R> + Send + Sync>;

/// A named job: a [`Cron`] expression, the effect to run at each fire time, and its policies and
/// hooks. Register it in a [`CronRegistry`].
pub struct CronJob<R = HNil> {
  name: Arc<str>,
  cron: Cron,
  task: Task<R>,
  overlap: OverlapPolicy,
  missed: MissedRunPolicy,
  on_success: Vec<Hook<R>>,
  on_failure: Vec<Hook<R>>,
  history_limit: usize,
}

impl<R> Clone for CronJob<R> {
  fn clone(&self) -> Self {
    Self {
      name: Arc::clone(&self.name),
      cron: self.cron.clone(),
      task: Arc::clone(&self.task),
      overlap: self.overlap,
      missed: self.missed,
      on_success: self.on_success.clone(),
      on_failure: self.on_failure.clone(),
      history_limit: self.history_limit,
    }
  }
}

impl<R> fmt::Debug for CronJob<R> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("CronJob")
      .field("name", &&*self.name)
      .field("cron", &self.cron)
      .field("overlap", &self.overlap)
      .field("missed", &self.missed)
      .finish_non_exhaustive()
  }
}

impl<R: Send + Sync + 'static> CronJob<R> {
  /// A job named `name` that runs `make()` at every fire time of `cron`. `make` is called once
  /// per run, when the run starts; a typed failure is recorded with its `Display` text.
  pub fn new<A, E, F>(name: impl Into<Arc<str>>, cron: Cron, make: F) -> Self
  where
    A: Send + 'static,
    E: fmt::Display + Send + 'static,
    F: Fn() -> Effect<A, E, R> + Send + Sync + 'static,
  {
    Self {
      name: name.into(),
      cron,
      task: Arc::new(move || make().map(|_| ()).map_err(|e| e.to_string())),
      overlap: OverlapPolicy::default(),
      missed: MissedRunPolicy::default(),
      on_success: Vec::new(),
      on_failure: Vec::new(),
      history_limit: DEFAULT_HISTORY,
    }
  }

  /// Sets the [`OverlapPolicy`] (default `Skip`).
  pub fn overlap(mut self, policy: OverlapPolicy) -> Self {
    self.overlap = policy;
    self
  }

  /// Sets the [`MissedRunPolicy`] (default `Skip`).
  pub fn missed_runs(mut self, policy: MissedRunPolicy) -> Self {
    self.missed = policy;
    self
  }

  /// Adds a hook run after each successful run (a defect in it is ignored).
  pub fn on_success(
    mut self,
    hook: impl Fn(&CronRun) -> Effect<(), Never, R> + Send + Sync + 'static,
  ) -> Self {
    self.on_success.push(Arc::new(hook));
    self
  }

  /// Adds a hook run after each failed run (a defect in it is ignored).
  pub fn on_failure(
    mut self,
    hook: impl Fn(&CronRun) -> Effect<(), Never, R> + Send + Sync + 'static,
  ) -> Self {
    self.on_failure.push(Arc::new(hook));
    self
  }

  /// Keeps the last `limit` runs (and skips) in the history (default 100).
  pub fn history_limit(mut self, limit: usize) -> Self {
    self.history_limit = limit;
    self
  }

  /// The job's name.
  pub fn name(&self) -> &str {
    &self.name
  }

  /// The job's expression.
  pub fn cron(&self) -> &Cron {
    &self.cron
  }
}

/// Per-job state shared by the scheduler fiber, its runs and [`CronScheduler`].
struct JobState {
  name: Arc<str>,
  limit: usize,
  history: Mutex<VecDeque<CronRun>>,
  running: AtomicUsize,
}

impl JobState {
  fn record(&self, run: CronRun) {
    if self.limit == 0 {
      return;
    }
    let mut h = self.history.lock().unwrap_or_else(PoisonError::into_inner);
    if h.len() >= self.limit {
      h.pop_front();
    }
    h.push_back(run);
  }
}

struct Entry<R> {
  job: CronJob<R>,
  state: Arc<JobState>,
}

// ── registry ────────────────────────────────────────────────────────────────────────────────

/// The central, declarative list of named [`CronJob`]s sharing an environment `R`.
///
/// Build it once (`schedule` / `with`), then run every job with [`run`](Self::run) or provide a
/// [`CronScheduler`] for the lifetime of a [`Layer`] with [`layer`](Self::layer). Cloning shares
/// the jobs and their history.
pub struct CronRegistry<R = HNil> {
  entries: Vec<Arc<Entry<R>>>,
}

impl<R> Clone for CronRegistry<R> {
  fn clone(&self) -> Self {
    Self {
      entries: self.entries.clone(),
    }
  }
}

impl<R> Default for CronRegistry<R> {
  fn default() -> Self {
    Self {
      entries: Vec::new(),
    }
  }
}

impl<R> fmt::Debug for CronRegistry<R> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_list()
      .entries(self.entries.iter().map(|e| &e.job))
      .finish()
  }
}

impl<R: Send + Sync + 'static> CronRegistry<R> {
  /// An empty registry.
  pub fn new() -> Self {
    Self::default()
  }

  /// Registers `job`; fails with [`CronError::DuplicateJob`] when its name is taken.
  pub fn with(mut self, job: CronJob<R>) -> Result<Self, CronError> {
    if self.entries.iter().any(|e| e.job.name == job.name) {
      return Err(CronError::DuplicateJob(job.name.to_string()));
    }
    let state = Arc::new(JobState {
      name: Arc::clone(&job.name),
      limit: job.history_limit,
      history: Mutex::new(VecDeque::new()),
      running: AtomicUsize::new(0),
    });
    self.entries.push(Arc::new(Entry { job, state }));
    Ok(self)
  }

  /// Parses `expression` (in UTC) and registers a job with the default policies: shorthand
  /// for `with(CronJob::new(name, Cron::parse(expression)?, make))`.
  pub fn schedule<A, E, F>(
    self,
    name: impl Into<Arc<str>>,
    expression: &str,
    make: F,
  ) -> Result<Self, CronError>
  where
    A: Send + 'static,
    E: fmt::Display + Send + 'static,
    F: Fn() -> Effect<A, E, R> + Send + Sync + 'static,
  {
    let cron = Cron::parse(expression)?;
    self.with(CronJob::new(name, cron, make))
  }

  /// The number of registered jobs.
  pub fn len(&self) -> usize {
    self.entries.len()
  }

  /// `true` when no job is registered.
  pub fn is_empty(&self) -> bool {
    self.entries.is_empty()
  }

  /// A handle onto the jobs' run history (shared with every run of this registry).
  pub fn scheduler(&self) -> CronScheduler {
    CronScheduler {
      jobs: self.entries.iter().map(|e| Arc::clone(&e.state)).collect(),
    }
  }

  /// Runs every job's scheduler fiber concurrently, reading fire times on `clock`. It completes
  /// only when no job will ever fire again; interrupt it to stop the jobs (runs in flight are
  /// interrupted and awaited).
  pub fn run(&self, clock: WallClock) -> Effect<(), Never, R> {
    let loops: Vec<_> = self
      .entries
      .iter()
      .map(|e| start_loop(Arc::clone(e), clock.clone()))
      .collect();
    all(loops, Concurrency::Unbounded).map(|_| ())
  }

  /// A layer that starts every job when it is built and provides the [`CronScheduler`]; the
  /// jobs stop (in-flight runs are interrupted and awaited) when the layer's scope closes.
  pub fn layer(&self, clock: WallClock) -> Layer<HCons<CronScheduler, HNil>, Never, R>
  where
    R: HList,
  {
    let registry = self.clone();
    Layer::scoped(move |scope: Scope| {
      let started = registry
        .entries
        .iter()
        .fold(succeed::<(), Never, R>(()), |acc, e| {
          let fiber = scope.fork_in(start_loop(Arc::clone(e), clock.clone()));
          acc.then(fiber.map(|_| ()))
        });
      let scheduler = registry.scheduler();
      started.map(move |()| HCons::new(scheduler, HNil))
    })
  }
}

// ── scheduler handle ────────────────────────────────────────────────────────────────────────

/// Read access to a [`CronRegistry`]'s jobs and their run history.
#[derive(Clone)]
pub struct CronScheduler {
  jobs: Arc<[Arc<JobState>]>,
}

impl fmt::Debug for CronScheduler {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("CronScheduler")
      .field("jobs", &self.job_names())
      .finish()
  }
}

impl CronScheduler {
  /// The registered job names, in registration order.
  pub fn job_names(&self) -> Vec<Arc<str>> {
    self.jobs.iter().map(|j| Arc::clone(&j.name)).collect()
  }

  /// The recorded runs and skips of `job`, oldest first (`None` for an unknown job).
  pub fn history(&self, job: &str) -> Option<Vec<CronRun>> {
    let state = self.state(job)?;
    let h = state.history.lock().unwrap_or_else(PoisonError::into_inner);
    Some(h.iter().cloned().collect())
  }

  /// The most recent record of `job`.
  pub fn last_run(&self, job: &str) -> Option<CronRun> {
    let state = self.state(job)?;
    let h = state.history.lock().unwrap_or_else(PoisonError::into_inner);
    h.back().cloned()
  }

  /// How many runs of `job` are in flight (`None` for an unknown job).
  pub fn running(&self, job: &str) -> Option<usize> {
    Some(self.state(job)?.running.load(Ordering::SeqCst))
  }

  fn state(&self, job: &str) -> Option<&Arc<JobState>> {
    self.jobs.iter().find(|j| &*j.name == job)
  }
}

// ── the scheduler fiber ─────────────────────────────────────────────────────────────────────

/// A job's scheduler loop, starting from the current civil instant (earlier fire times are not
/// due).
fn start_loop<R: Send + Sync + 'static>(
  entry: Arc<Entry<R>>,
  clock: WallClock,
) -> Effect<(), Never, R> {
  clock.now().flat_map(move |now| job_loop(entry, clock, now))
}

/// One wake-up: dispatch the fire times due in `(last, now]`, or sleep until the next one.
fn job_loop<R: Send + Sync + 'static>(
  entry: Arc<Entry<R>>,
  clock: WallClock,
  last: UtcDateTime,
) -> Effect<(), Never, R> {
  clock.now().flat_map(move |now| {
    let keep = match entry.job.missed {
      MissedRunPolicy::Skip => 1,
      MissedRunPolicy::CatchUp { max } => max.saturating_add(1),
    };
    let mut due: VecDeque<ZonedDateTime> = VecDeque::new();
    let mut cursor = last;
    let upcoming = loop {
      match entry.job.cron.next_after(cursor) {
        Some(fire) if fire.0.timestamp() <= now.0 => {
          cursor = UtcDateTime(fire.0.timestamp());
          due.push_back(fire);
          if due.len() > keep
            && let Some(old) = due.pop_front()
          {
            let skip = CronRun::skipped(&entry.job.name, old, CronSkip::Missed);
            entry.state.record(skip);
          }
        }
        other => break other,
      }
    };
    if due.is_empty() {
      return match upcoming {
        None => succeed(()),
        Some(next) => sleep(delay_until(now, &next)).then(job_loop(entry, clock, last)),
      };
    }
    let next_last = cursor;
    dispatch(&entry, &clock, due.into()).then(job_loop(entry, clock, next_last))
  })
}

/// Starts one batch for `fires` (run in order) as a child fiber, or skips them on overlap.
fn dispatch<R: Send + Sync + 'static>(
  entry: &Arc<Entry<R>>,
  clock: &WallClock,
  fires: Vec<ZonedDateTime>,
) -> Effect<(), Never, R> {
  let state = &entry.state;
  if entry.job.overlap == OverlapPolicy::Skip && state.running.load(Ordering::SeqCst) > 0 {
    for fire in fires {
      state.record(CronRun::skipped(&entry.job.name, fire, CronSkip::Overlap));
    }
    return succeed(());
  }
  state.running.fetch_add(1, Ordering::SeqCst);
  let batch = fires.into_iter().fold(succeed(()), |acc, fire| {
    acc.then(run_once(Arc::clone(entry), clock.clone(), fire))
  });
  let done = Arc::clone(state);
  batch
    .ensuring(sync(move || {
      done.running.fetch_sub(1, Ordering::SeqCst);
    }))
    .fork()
    .map(|_| ())
}

/// Runs the job once for `fire`, records it, then runs the matching hooks.
fn run_once<R: Send + Sync + 'static>(
  entry: Arc<Entry<R>>,
  clock: WallClock,
  fire: ZonedDateTime,
) -> Effect<(), Never, R> {
  let task = Arc::clone(&entry.job.task);
  let end = clock.clone();
  clock.now().flat_map(move |started| {
    suspend(move || task())
      .exit::<Never>()
      .flat_map(move |exit| {
        end.now().flat_map(move |finished| {
          let outcome = match exit {
            Exit::Success(()) => CronOutcome::Succeeded,
            Exit::Failure(cause) => CronOutcome::Failed(render(&cause)),
          };
          let run = CronRun {
            job: Arc::clone(&entry.job.name),
            scheduled: fire,
            started: Some(started),
            finished: Some(finished),
            outcome,
          };
          entry.state.record(run.clone());
          let hooks = if run.is_success() {
            &entry.job.on_success
          } else {
            &entry.job.on_failure
          };
          hooks.iter().fold(succeed(()), |acc, hook| {
            let (hook, run) = (Arc::clone(hook), run.clone());
            acc.then(suspend(move || hook(&run)).exit::<Never>().map(|_| ()))
          })
        })
      })
  })
}

fn render(cause: &Cause<String>) -> String {
  match cause {
    Cause::Fail(e) => e.clone(),
    Cause::Die(d) => format!("defect: {d}"),
    Cause::Interrupt(_) => "interrupted".to_string(),
    Cause::Both(a, b) | Cause::Then(a, b) => format!("{}; {}", render(a), render(b)),
  }
}
