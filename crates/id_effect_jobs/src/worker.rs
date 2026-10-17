//! [`Worker`]: runs the handlers of a [`JobRegistry`] against the [`JobQueueService`] in its
//! environment, with a concurrency limit per queue, a lease kept alive by a heartbeat, rescue of
//! crashed workers' jobs, and a drain on shutdown (the model is on [`Worker`]).

use std::fmt;
use std::hash::{BuildHasher, Hasher};
use std::num::NonZeroU16;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use id_effect::schema::unknown_from_serde_json;
use id_effect::{
  Cause, Concurrency, Effect, Exit, FiberHandle, HCons, HList, HNil, Layer, Never, Permit,
  Schedule, Scope, Selector, Semaphore, UtcDateTime, WallClock, all, fail, need, sleep, succeed,
  suspend, sync,
};

use crate::job::{JobContext, JobFailure, JobId, JobRegistry, Queue, default_retry, retry_delay};
use crate::queue::{Claimed, Fenced, JobError, JobQueueService, WakeSignal, earlier};

/// The most jobs one poll claims.
const MAX_BATCH: usize = 32;
/// How often a draining worker checks whether its runs finished.
const DRAIN_STEP: Duration = Duration::from_millis(10);
/// The first delay after a failed poll (doubling up to [`MAX_ERROR_BACKOFF`]).
const ERROR_BACKOFF: Duration = Duration::from_millis(100);
const MAX_ERROR_BACKOFF: Duration = Duration::from_secs(30);

/// The worker's identity in the `locked_by` column: unique per worker process (default
/// `host:pid:random`).
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct WorkerId(Arc<str>);

impl WorkerId {
  /// The id `id`.
  pub fn new(id: impl Into<Arc<str>>) -> Self {
    Self(id.into())
  }

  /// `host:pid:random`, with the host from `$HOSTNAME` (or `localhost`).
  pub fn generate() -> Self {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u64(NEXT.fetch_add(1, Ordering::Relaxed));
    let host = std::env::var("HOSTNAME")
      .ok()
      .filter(|h| !h.is_empty())
      .unwrap_or_else(|| "localhost".to_owned());
    let random = h.finish() & 0xffff_ffff;
    Self::new(format!("{host}:{}:{random:08x}", std::process::id()))
  }

  /// The id.
  pub fn as_str(&self) -> &str {
    &self.0
  }
}

impl fmt::Debug for WorkerId {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "WorkerId({:?})", &*self.0)
  }
}

impl fmt::Display for WorkerId {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(&self.0)
  }
}

/// How an attempt ended, as a [`JobObserver`] sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Outcome {
  /// The job succeeded.
  Succeeded,
  /// The attempt failed; the job runs again after `delay`.
  Retrying {
    /// The failure's text.
    error: String,
    /// The backoff.
    delay: Duration,
  },
  /// The attempt failed and the job is `dead`.
  Dead {
    /// The failure's text.
    error: String,
  },
}

/// Hooks for metrics and the console; every method defaults to doing nothing. Called on the
/// worker's fibers: keep them cheap and non-blocking.
pub trait JobObserver: Send + Sync + 'static {
  /// An attempt of `job` starts.
  fn claimed(&self, _job: &JobContext) {}

  /// An attempt of `job` ended with `outcome` after `elapsed` (on the worker's clock) and was
  /// recorded.
  fn finished(&self, _job: &JobContext, _outcome: &Outcome, _elapsed: Duration) {}

  /// The outcome of job `id` was dropped: its lease had expired and it was rescued.
  fn lost(&self, _id: JobId) {}

  /// A store operation of the worker failed (it is retried).
  fn store_error(&self, _error: &JobError) {}
}

/// The default [`JobObserver`]: observes nothing.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoopObserver;

impl JobObserver for NoopObserver {}

/// A [`Worker`]'s settings.
#[derive(Clone)]
pub struct WorkerConfig {
  /// The worker's id (default [`WorkerId::generate`]).
  pub id: WorkerId,
  /// The longest wait between polls of an idle queue (default 1 s).
  pub poll_interval: Duration,
  /// How long a claimed job is leased to the worker before another worker may rescue it
  /// (default 5 min; the heartbeat renews it every third of it).
  pub lease: Duration,
  /// How often the worker rescues expired leases and prunes (default 30 s).
  pub rescue_every: Duration,
  /// How long a shutdown waits for the runs in flight before interrupting them (default 30 s).
  pub drain_timeout: Duration,
  /// Delete finished jobs this old on every rescue tick (default 7 days; `None` keeps them).
  pub prune_after: Option<Duration>,
  /// The clock of the worker's own times (attempt durations, pruning).
  pub clock: WallClock,
  /// Receives every attempt's outcome (default [`NoopObserver`]).
  pub observer: Arc<dyn JobObserver>,
}

impl Default for WorkerConfig {
  fn default() -> Self {
    Self {
      id: WorkerId::generate(),
      poll_interval: Duration::from_secs(1),
      lease: Duration::from_secs(300),
      rescue_every: Duration::from_secs(30),
      drain_timeout: Duration::from_secs(30),
      prune_after: Some(Duration::from_secs(7 * 24 * 3600)),
      clock: WallClock::system(),
      observer: Arc::new(NoopObserver),
    }
  }
}

impl fmt::Debug for WorkerConfig {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("WorkerConfig")
      .field("id", &self.id)
      .field("poll_interval", &self.poll_interval)
      .field("lease", &self.lease)
      .field("rescue_every", &self.rescue_every)
      .field("drain_timeout", &self.drain_timeout)
      .field("prune_after", &self.prune_after)
      .field("clock", &self.clock)
      .finish_non_exhaustive()
  }
}

/// What [`Worker::run_until_idle`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DrainReport {
  /// Attempts that succeeded.
  pub succeeded: u64,
  /// Attempts that failed and will run again.
  pub retrying: u64,
  /// Attempts that failed and left the job `dead`.
  pub dead: u64,
  /// Outcomes dropped because the job's lease had expired.
  pub lost: u64,
}

impl DrainReport {
  /// Every attempt recorded.
  pub fn runs(&self) -> u64 {
    self.succeeded + self.retrying + self.dead + self.lost
  }
}

/// The capability [`Worker::layer`] provides while the worker runs.
#[derive(Clone, Debug)]
pub struct WorkerHandle {
  id: WorkerId,
  in_flight: Arc<AtomicUsize>,
}

impl WorkerHandle {
  /// The worker's id.
  pub fn id(&self) -> &WorkerId {
    &self.id
  }

  /// How many attempts are running now.
  pub fn in_flight(&self) -> usize {
    self.in_flight.load(Ordering::SeqCst)
  }
}

/// Runs the jobs of a [`JobRegistry`].
///
/// - **Polling.** Each queue has one poll fiber and a [`Semaphore`] of `concurrency` permits. It
///   claims as many jobs as there are free permits (at most 32), runs each in its own fiber, and
///   polls again at once when the batch was full, else after
///   [`poll_interval`](WorkerConfig::poll_interval) on the executor clock (virtual on a
///   `TestExecutor`) or as soon as the store's [`WakeSignal`] reports a job inserted in this
///   process. There is no `LISTEN/NOTIFY`: it needs a connection outside the pool, breaks under
///   transaction pooling and loses notifications across reconnects, so polling stays the source
///   of truth.
/// - **Leases.** A claimed job is leased to the worker for [`lease`](WorkerConfig::lease); a
///   heartbeat every `lease / 3` extends the lease of every job it runs. Every worker rescues the
///   jobs whose lease expired (a crashed or hung worker) every
///   [`rescue_every`](WorkerConfig::rescue_every): they run again (at least once delivery). A
///   late outcome of a rescued job is dropped ([`Fenced::Lost`]).
/// - **Outcomes.** A success completes the job. A typed failure, a defect or a panic in the
///   handler, a timeout, or an unknown job name fails the attempt: the job runs again after the
///   [`Job::retry`](crate::Job::retry) delay, or is `dead` once its attempts are exhausted, its
///   schedule is done, or the failure is a [`JobFailure::Discard`] (a payload that no longer
///   decodes). Each attempt runs in a span `job.run` with `job.name`, `job.queue`, `job.id` and
///   `job.attempt`.
/// - **Errors** of the store in the poll loop are reported to the [`JobObserver`] and retried
///   with backoff (100 ms doubling to 30 s); they never end the worker.
/// - **Shutdown.** Interrupting [`Worker::run`] (or closing the scope of [`Worker::layer`])
///   stops claiming, waits up to [`drain_timeout`](WorkerConfig::drain_timeout) for the runs in
///   flight, interrupts the rest, and puts their jobs back to `scheduled` with the attempt
///   refunded.
///
/// The handlers' environment `R` must hold the [`JobQueueService`] the worker claims from, so
/// the worker runs under the same layers as the rest of the application.
pub struct Worker<R> {
  registry: JobRegistry<R>,
  queues: Vec<(Queue, NonZeroU16)>,
  config: WorkerConfig,
}

impl<R> Clone for Worker<R> {
  fn clone(&self) -> Self {
    Self {
      registry: self.registry.clone(),
      queues: self.queues.clone(),
      config: self.config.clone(),
    }
  }
}

impl<R> fmt::Debug for Worker<R> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("Worker")
      .field("registry", &self.registry)
      .field("queues", &self.queues)
      .field("config", &self.config)
      .finish()
  }
}

impl<R: HList> Worker<R> {
  /// A worker for `registry` with the default [`WorkerConfig`]. Without a [`queue`](Self::queue)
  /// it serves the `default` queue with concurrency 10.
  pub fn new(registry: JobRegistry<R>) -> Self {
    Self {
      registry,
      queues: Vec::new(),
      config: WorkerConfig::default(),
    }
  }

  /// Serves `name` with at most `concurrency` attempts at a time in this worker (at least 1).
  /// Serving a queue again replaces its concurrency.
  pub fn queue(mut self, name: &str, concurrency: u16) -> Self {
    let n = NonZeroU16::new(concurrency).unwrap_or(NonZeroU16::MIN);
    let queue = Queue::new(name);
    self.queues.retain(|(q, _)| *q != queue);
    self.queues.push((queue, n));
    self
  }

  /// Replaces the configuration.
  pub fn with_config(mut self, config: WorkerConfig) -> Self {
    self.config = config;
    self
  }

  /// The configuration.
  pub fn config(&self) -> &WorkerConfig {
    &self.config
  }

  /// The queues served, with their concurrency.
  pub fn queues(&self) -> Vec<(Queue, u16)> {
    self
      .served()
      .into_iter()
      .map(|(q, n)| (q, n.get()))
      .collect()
  }

  fn served(&self) -> Vec<(Queue, NonZeroU16)> {
    if self.queues.is_empty() {
      let ten = NonZeroU16::new(10).unwrap_or(NonZeroU16::MIN);
      vec![(Queue::default(), ten)]
    } else {
      self.queues.clone()
    }
  }

  fn shared(self, store: JobQueueService, in_flight: Arc<AtomicUsize>) -> Arc<Shared<R>> {
    Arc::new(Shared {
      queues: self.served(),
      registry: self.registry,
      config: self.config,
      store,
      stopping: AtomicBool::new(false),
      in_flight,
      runs: Mutex::default(),
    })
  }

  /// Runs until interrupted; interrupting it drains (see [`Worker`]).
  pub fn run<I>(self) -> Effect<(), Never, R>
  where
    R: Selector<JobQueueService, I>,
  {
    self.start(Arc::default())
  }

  fn start<I>(self, in_flight: Arc<AtomicUsize>) -> Effect<(), Never, R>
  where
    R: Selector<JobQueueService, I>,
  {
    need::<JobQueueService>().flat_map(move |store| {
      let shared = self.shared((*store).clone(), in_flight);
      let started = shared
        .queues
        .iter()
        .fold(succeed::<(), Never, R>(()), |acc, (queue, n)| {
          let permits = Semaphore::new(usize::from(n.get()));
          let poll = poll_loop(Arc::clone(&shared), queue.clone(), permits, 0);
          acc.then(poll.fork().map(|_| ()))
        });
      let heartbeat = heartbeat_loop(Arc::clone(&shared)).fork().map(|_| ());
      started
        .then(heartbeat)
        .then(maintain(Arc::clone(&shared)))
        .ensuring(drain(shared))
    })
  }

  /// A layer that starts the worker when it is built and provides its [`WorkerHandle`]; closing
  /// the layer's scope drains the worker.
  pub fn layer<I>(self) -> Layer<HCons<WorkerHandle, HNil>, Never, R>
  where
    R: Selector<JobQueueService, I>,
    I: 'static,
  {
    Layer::scoped(move |scope: Scope| {
      let worker = self.clone();
      let handle = WorkerHandle {
        id: worker.config.id.clone(),
        in_flight: Arc::default(),
      };
      let in_flight = Arc::clone(&handle.in_flight);
      scope
        .fork_in(worker.start(in_flight))
        .map(move |_| HCons::new(handle, HNil))
    })
  }

  /// Claims and runs jobs until no job is runnable, then returns what ran: for tests and batch
  /// programs. Store errors fail it.
  pub fn run_until_idle<I>(self) -> Effect<DrainReport, JobError, R>
  where
    R: Selector<JobQueueService, I>,
  {
    need::<JobQueueService>().flat_map(move |store| {
      let shared = self.shared((*store).clone(), Arc::default());
      idle_pass(shared, DrainReport::default())
    })
  }
}

/// Per-run state shared by a worker's fibers.
struct Shared<R> {
  queues: Vec<(Queue, NonZeroU16)>,
  registry: JobRegistry<R>,
  config: WorkerConfig,
  store: JobQueueService,
  stopping: AtomicBool,
  in_flight: Arc<AtomicUsize>,
  runs: Mutex<Vec<FiberHandle<(), Never>>>,
}

impl<R> Shared<R> {
  fn worker(&self) -> &str {
    self.config.id.as_str()
  }

  fn stopping(&self) -> bool {
    self.stopping.load(Ordering::SeqCst)
  }

  fn runs(&self) -> MutexGuard<'_, Vec<FiberHandle<(), Never>>> {
    self.runs.lock().unwrap_or_else(PoisonError::into_inner)
  }
}

/// How one attempt ended for the worker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Settled {
  Succeeded,
  Retrying,
  Dead,
  Lost,
  /// Interrupted (a drain): nothing recorded; the release puts the job back.
  Interrupted,
  /// Recording the outcome failed; the lease expires and the job is rescued.
  StoreFailed,
}

/// A store effect in the worker's environment.
fn lift<A: Send + 'static, R: HList>(eff: Effect<A, JobError>) -> Effect<A, JobError, R> {
  eff.narrow()
}

/// `eff` with its failure reported to the observer instead.
fn quiet<A: Send + 'static, R: HList>(
  shared: &Arc<Shared<R>>,
  eff: Effect<A, JobError>,
) -> Effect<(), Never, R> {
  let s = Arc::clone(shared);
  lift(eff).map(|_| ()).catch(move |e| {
    s.config.observer.store_error(&e);
    succeed(())
  })
}

/// The failure an attempt's cause stands for; `None` when it was interrupted.
fn failure_of(cause: &Cause<JobFailure>) -> Option<JobFailure> {
  match cause {
    Cause::Fail(f) => Some(f.clone()),
    Cause::Die(d) => Some(JobFailure::Retry(format!("defect: {d}"))),
    Cause::Interrupt(_) => None,
    Cause::Both(a, b) | Cause::Then(a, b) => failure_of(a).or_else(|| failure_of(b)),
  }
}

/// Runs one claimed attempt and records its outcome.
fn execute<R: HList>(shared: Arc<Shared<R>>, job: Claimed) -> Effect<Settled, Never, R> {
  let ctx = JobContext {
    id: job.id,
    name: job.name.clone(),
    queue: job.queue.clone(),
    attempt: job.attempt,
    max_attempts: job.max_attempts,
    inserted_at: job.inserted_at,
  };
  let attrs = [
    ("job.name", job.name.clone()),
    ("job.queue", job.queue.to_string()),
    ("job.id", job.id.to_string()),
    ("job.attempt", job.attempt.to_string()),
  ];
  let entry = shared.registry.get(&job.name);
  let payload = job.payload;
  let clock = shared.config.clock.clone();
  clock.now().flat_map(move |started| {
    shared.config.observer.claimed(&ctx);
    let (attempt, retry): (Effect<(), JobFailure, R>, Schedule<(), JobFailure>) = match entry {
      Some(entry) => {
        let (run, ctx) = (Arc::clone(&entry.run), ctx.clone());
        let attempt = suspend(move || run(unknown_from_serde_json(payload), ctx));
        (attempt, entry.retry.clone())
      }
      None => {
        let unknown = format!("no handler is registered for job `{}`", ctx.name);
        (fail(JobFailure::Retry(unknown)), default_retry())
      }
    };
    attempt
      .with_span_attrs("job.run", attrs)
      .exit::<Never>()
      .flat_map(move |exit| {
        let failure = match exit {
          Exit::Success(()) => None,
          Exit::Failure(cause) => match failure_of(&cause) {
            Some(f) => Some(f),
            None => return succeed(Settled::Interrupted),
          },
        };
        settle(shared, ctx, started, failure, &retry)
      })
  })
}

/// Records the outcome of attempt `ctx` (`None`: it succeeded) and tells the observer.
fn settle<R: HList>(
  shared: Arc<Shared<R>>,
  ctx: JobContext,
  started: UtcDateTime,
  failure: Option<JobFailure>,
  retry: &Schedule<(), JobFailure>,
) -> Effect<Settled, Never, R> {
  let (record, outcome) = match failure {
    None => (
      shared.store.complete(ctx.id, shared.worker()),
      Outcome::Succeeded,
    ),
    Some(f) => {
      let delay = if f.is_discard() || ctx.attempt >= ctx.max_attempts {
        None
      } else {
        retry_delay(retry, &f, ctx.attempt)
      };
      let error = f.message().to_owned();
      let outcome = match delay {
        Some(delay) => Outcome::Retrying { error, delay },
        None => Outcome::Dead { error },
      };
      (
        shared.store.fail(ctx.id, shared.worker(), &f, delay),
        outcome,
      )
    }
  };
  let clock = shared.config.clock.clone();
  lift::<_, R>(record).exit::<Never>().flat_map(move |exit| {
    clock.now().map(move |finished| {
      let observer = &shared.config.observer;
      match exit {
        Exit::Success(Fenced::Applied) => {
          observer.finished(&ctx, &outcome, started.distance_duration(&finished));
          match outcome {
            Outcome::Succeeded => Settled::Succeeded,
            Outcome::Retrying { .. } => Settled::Retrying,
            Outcome::Dead { .. } => Settled::Dead,
          }
        }
        Exit::Success(Fenced::Lost) => {
          observer.lost(ctx.id);
          Settled::Lost
        }
        Exit::Failure(cause) => {
          if let Cause::Fail(e) = &cause {
            observer.store_error(e);
          }
          Settled::StoreFailed
        }
      }
    })
  })
}

/// Starts `job` in its own fiber, holding `permit` until it ends.
fn spawn_run<R: HList>(
  shared: Arc<Shared<R>>,
  job: Claimed,
  permit: Permit,
) -> Effect<(), Never, R> {
  suspend(move || {
    shared.in_flight.fetch_add(1, Ordering::SeqCst);
    let done = Arc::clone(&shared);
    let release = sync(move || {
      drop(permit);
      done.in_flight.fetch_sub(1, Ordering::SeqCst);
    });
    let runs = Arc::clone(&shared);
    // A daemon: the drain, not the poll fiber's exit, decides when a run is interrupted.
    execute(shared, job)
      .map(|_| ())
      .ensuring(release)
      .fork_daemon()
      .map(move |handle| {
        let mut live = runs.runs();
        live.retain(|h| !h.is_done());
        live.push(handle);
      })
  })
}

/// The delay after `errors` consecutive failed polls.
fn backoff(errors: u32) -> Duration {
  let factor = 1u32.checked_shl(errors).unwrap_or(u32::MAX);
  ERROR_BACKOFF.saturating_mul(factor).min(MAX_ERROR_BACKOFF)
}

/// Waits `poll` or until `wake` reports a job inserted since generation `seen`.
fn idle<R: HList>(poll: Duration, wake: Option<(WakeSignal, u64)>) -> Effect<(), Never, R> {
  match wake {
    Some((signal, seen)) => sleep(poll).race(signal.wait_since(seen)),
    None => sleep(poll),
  }
}

/// One queue's poll fiber.
fn poll_loop<R: HList>(
  shared: Arc<Shared<R>>,
  queue: Queue,
  permits: Semaphore,
  errors: u32,
) -> Effect<(), Never, R> {
  suspend(move || {
    if shared.stopping() {
      return succeed(());
    }
    let free = permits.clone();
    permits
      .acquire::<Never, R>()
      .flat_map(move |first| free.available().map(move |more| (first, more)))
      .flat_map(move |(first, more)| {
        if shared.stopping() {
          return succeed(());
        }
        let limit = (more + 1).min(MAX_BATCH);
        let wake = shared
          .store
          .wake_signal()
          .map(|signal| (signal.generation(), signal))
          .map(|(seen, signal)| (signal, seen));
        let claim = shared.store.claim(
          &queue,
          shared.worker(),
          u16::try_from(limit).unwrap_or(1),
          shared.config.lease,
        );
        lift::<_, R>(claim)
          .exit::<Never>()
          .flat_map(move |exit| match exit {
            Exit::Success(jobs) => {
              let full = jobs.len() == limit;
              let mut first = Some(first);
              let started = jobs
                .into_iter()
                .fold(succeed::<(), Never, R>(()), |acc, job| {
                  let s = Arc::clone(&shared);
                  match first.take() {
                    Some(permit) => acc.then(spawn_run(s, job, permit)),
                    None => acc.then(
                      permits
                        .acquire()
                        .flat_map(move |permit| spawn_run(s, job, permit)),
                    ),
                  }
                });
              // No job: give the permit back before waiting.
              drop(first);
              let poll = shared.config.poll_interval;
              let next = poll_loop(shared, queue, permits, 0);
              if full {
                started.then(next)
              } else {
                started.then(idle(poll, wake)).then(next)
              }
            }
            Exit::Failure(cause) => {
              drop(first);
              if let Cause::Fail(e) = &cause {
                shared.config.observer.store_error(e);
              }
              let next = poll_loop(shared, queue, permits, errors.saturating_add(1));
              sleep(backoff(errors)).then(next)
            }
          })
      })
  })
}

/// Extends the lease of the worker's running jobs every `lease / 3`.
fn heartbeat_loop<R: HList>(shared: Arc<Shared<R>>) -> Effect<(), Never, R> {
  suspend(move || {
    let lease = shared.config.lease;
    let every = (lease / 3).max(Duration::from_millis(10));
    let beat = quiet(&shared, shared.store.heartbeat(shared.worker(), lease));
    sleep(every).then(beat).then(heartbeat_loop(shared))
  })
}

/// Rescues expired leases and prunes, now and every `rescue_every`.
fn maintain<R: HList>(shared: Arc<Shared<R>>) -> Effect<(), Never, R> {
  suspend(move || {
    let rescue = quiet(&shared, shared.store.rescue_expired());
    let prune = match shared.config.prune_after {
      None => succeed(()),
      Some(age) => {
        let s = Arc::clone(&shared);
        shared.config.clock.now().flat_map(move |now| {
          let cutoff = earlier(now, age);
          quiet(&s, s.store.prune(cutoff))
        })
      }
    };
    let every = shared.config.rescue_every;
    rescue.then(prune).then(sleep(every)).then(maintain(shared))
  })
}

/// Stops claiming, waits for the runs in flight, interrupts the rest, releases their jobs.
fn drain<R: HList>(shared: Arc<Shared<R>>) -> Effect<(), Never, R> {
  let steps = (shared.config.drain_timeout.as_nanos() / DRAIN_STEP.as_nanos()).max(1);
  let steps = u64::try_from(steps).unwrap_or(u64::MAX);
  let stop = Arc::clone(&shared);
  let (waited, interrupted, released) = (
    Arc::clone(&shared),
    Arc::clone(&shared),
    Arc::clone(&shared),
  );
  sync(move || stop.stopping.store(true, Ordering::SeqCst))
    .then(wait_idle(waited, steps))
    .then(interrupt_runs(interrupted))
    .then(suspend(move || {
      quiet(&released, released.store.release(released.worker()))
    }))
}

fn wait_idle<R: HList>(shared: Arc<Shared<R>>, left: u64) -> Effect<(), Never, R> {
  suspend(move || {
    if left == 0 || shared.in_flight.load(Ordering::SeqCst) == 0 {
      succeed(())
    } else {
      sleep(DRAIN_STEP).then(wait_idle(shared, left - 1))
    }
  })
}

fn interrupt_runs<R: HList>(shared: Arc<Shared<R>>) -> Effect<(), Never, R> {
  suspend(move || {
    let runs = std::mem::take(&mut *shared.runs());
    runs.into_iter().fold(succeed(()), |acc, run| {
      acc.then(run.interrupt::<Never, R>().map(|_| ()))
    })
  })
}

/// Claims every served queue until nothing is runnable ([`Worker::run_until_idle`]).
fn idle_pass<R: HList>(
  shared: Arc<Shared<R>>,
  report: DrainReport,
) -> Effect<DrainReport, JobError, R> {
  let claims = shared.queues.iter().fold(
    succeed::<Vec<Claimed>, JobError, R>(Vec::new()),
    |acc, (queue, n)| {
      let limit = n.get().min(MAX_BATCH as u16);
      let claim = lift::<_, R>(shared.store.claim(
        queue,
        shared.worker(),
        limit,
        shared.config.lease,
      ));
      acc.flat_map(move |mut jobs| {
        claim.map(move |mut more| {
          jobs.append(&mut more);
          jobs
        })
      })
    },
  );
  lift::<_, R>(shared.store.rescue_expired())
    .then(claims)
    .flat_map(move |jobs| {
      if jobs.is_empty() {
        return succeed(report);
      }
      let runs: Vec<_> = jobs
        .into_iter()
        .map(|job| execute(Arc::clone(&shared), job))
        .collect();
      all(runs, Concurrency::Unbounded)
        .widen()
        .flat_map(move |settled| {
          let mut report = report;
          for s in settled {
            match s {
              Settled::Succeeded => report.succeeded += 1,
              Settled::Retrying => report.retrying += 1,
              Settled::Dead => report.dead += 1,
              Settled::Lost => report.lost += 1,
              Settled::Interrupted | Settled::StoreFailed => {}
            }
          }
          idle_pass(shared, report)
        })
    })
}

#[cfg(test)]
mod tests {
  use std::sync::atomic::AtomicU32;

  use id_effect::{Executor, Schema, TestExecutor};

  use super::*;
  use crate::job::{Job, Priority};
  use crate::memory::{MemoryJobQueue, memory_job_queue_layer};
  use crate::queue::{
    JobFilter, JobOptions, JobQueue, JobQueueEnv, JobQueueExt, JobRecord, NewJob, later,
  };
  use crate::{JobState, job_queue_env};

  #[derive(Clone, Schema)]
  struct Nap {
    secs: i64,
    panic: bool,
  }

  impl Job for Nap {
    const NAME: &'static str = "test.nap";

    fn retry() -> Schedule<(), JobFailure> {
      Schedule::spaced(Duration::from_secs(5)).map(|_| ())
    }
  }

  fn nap(secs: i64) -> Nap {
    Nap { secs, panic: false }
  }

  fn registry(runs: &Arc<AtomicU32>) -> JobRegistry<JobQueueEnv> {
    let runs = Arc::clone(runs);
    JobRegistry::new()
      .register::<Nap, Never>(move |job, _| {
        let runs = Arc::clone(&runs);
        sync(move || {
          assert!(!job.panic, "the handler panicked");
          runs.fetch_add(1, Ordering::SeqCst);
        })
        .then(sleep(Duration::from_secs(job.secs.unsigned_abs())))
      })
      .unwrap()
  }

  struct Setup {
    exec: TestExecutor,
    store: MemoryJobQueue,
    clock: WallClock,
  }

  fn setup() -> Setup {
    let exec = TestExecutor::new();
    let clock = WallClock::anchored(exec.now(), UtcDateTime::unsafe_make(1_800_000_000_000));
    Setup {
      store: MemoryJobQueue::new(clock.clone()),
      clock,
      exec,
    }
  }

  impl Setup {
    fn config(&self) -> WorkerConfig {
      WorkerConfig {
        id: WorkerId::new("w1"),
        clock: self.clock.clone(),
        drain_timeout: Duration::from_secs(1),
        ..WorkerConfig::default()
      }
    }

    fn drive<A: Send + 'static>(&self, eff: Effect<A, JobError>) -> Result<A, JobError> {
      let slot = Arc::new(Mutex::new(None));
      let out = Arc::clone(&slot);
      let h = eff
        .exit::<Never>()
        .map(move |exit| *out.lock().unwrap() = Some(exit))
        .fork_root(Arc::new(self.exec.clone()));
      self.exec.run_until_stalled();
      assert!(
        h.is_done(),
        "the effect did not finish without advancing time"
      );
      match slot.lock().unwrap().take() {
        Some(Exit::Success(a)) => Ok(a),
        Some(Exit::Failure(Cause::Fail(e))) => Err(e),
        Some(Exit::Failure(c)) => panic!("unexpected cause: {c:?}"),
        None => panic!("no exit"),
      }
    }

    fn only(&self) -> JobRecord {
      let all = self.drive(self.store.list(JobFilter::default())).unwrap();
      assert_eq!(all.len(), 1);
      all[0].clone()
    }

    fn start(&self, worker: Worker<JobQueueEnv>) -> FiberHandle<(), Never> {
      worker
        .run()
        .provide_all(job_queue_env(self.store.clone()))
        .fork_root(Arc::new(self.exec.clone()))
    }
  }

  #[test]
  fn drain_releases_the_running_job_with_its_attempt_refunded() {
    let s = setup();
    let runs = Arc::new(AtomicU32::new(0));
    s.drive(s.store.enqueue(nap(3600), JobOptions::new()))
      .unwrap();
    let worker = Worker::new(registry(&runs)).with_config(s.config());
    let h = s.start(worker);
    s.exec.run_until_stalled();
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    let running = s.only();
    assert_eq!((running.state, running.attempts), (JobState::Running, 1));
    assert_eq!(running.locked_by.as_deref(), Some("w1"));
    h.interrupt_now();
    s.exec.advance(Duration::from_secs(2));
    assert!(h.is_done(), "drained within drain_timeout");
    let released = s.only();
    assert_eq!(
      (released.state, released.attempts, released.locked_by),
      (JobState::Scheduled, 0, None)
    );
  }

  #[test]
  fn a_run_that_finishes_within_the_drain_timeout_is_recorded() {
    let s = setup();
    let runs = Arc::new(AtomicU32::new(0));
    s.drive(s.store.enqueue(nap(1), JobOptions::new())).unwrap();
    let config = WorkerConfig {
      drain_timeout: Duration::from_secs(5),
      ..s.config()
    };
    let h = s.start(Worker::new(registry(&runs)).with_config(config));
    s.exec.run_until_stalled();
    assert_eq!(s.only().state, JobState::Running);
    // Interrupted mid-run: the drain waits for the run (1 s) instead of releasing it.
    h.interrupt_now();
    s.exec.advance(Duration::from_secs(2));
    assert!(h.is_done());
    assert_eq!(s.only().state, JobState::Succeeded);
  }

  #[test]
  fn an_undecodable_payload_is_dead_and_an_unknown_name_or_a_panic_retries() {
    let s = setup();
    let runs = Arc::new(AtomicU32::new(0));
    let bad = NewJob {
      payload: serde_json::json!({"secs": "soon"}),
      ..NewJob::of(nap(0), JobOptions::new())
    };
    let gone = NewJob {
      name: "test.removed".into(),
      ..NewJob::of(nap(0), JobOptions::new())
    };
    let panics = Nap {
      secs: 0,
      panic: true,
    };
    for job in [bad, gone, NewJob::of(panics, JobOptions::new())] {
      s.drive(s.store.enqueue_raw(job)).unwrap();
    }
    let worker = Worker::new(registry(&runs)).with_config(s.config());
    let report = s
      .drive(
        worker
          .run_until_idle()
          .provide_all(job_queue_env(s.store.clone())),
      )
      .unwrap();
    assert_eq!((report.dead, report.retrying, report.runs()), (1, 2, 3));
    let mut all = s.drive(s.store.list(JobFilter::default())).unwrap();
    all.reverse();
    let errors: Vec<_> = all.iter().map(|r| r.last_error.clone().unwrap()).collect();
    assert_eq!(all[0].state, JobState::Dead);
    assert!(
      errors[0].contains("does not decode") && errors[0].contains("secs"),
      "{errors:?}"
    );
    assert_eq!(all[1].state, JobState::Failed);
    assert!(errors[1].contains("no handler is registered for job `test.removed`"));
    assert_eq!(all[2].state, JobState::Failed);
    assert!(
      errors[2].starts_with("defect: ") && errors[2].contains("panicked"),
      "{errors:?}"
    );
    assert_eq!(
      all[2].run_at,
      later(s.clock.at(s.exec.now()), Duration::from_secs(5)),
      "the job's own retry schedule"
    );
  }

  #[derive(Default)]
  struct Counting {
    store_errors: AtomicU32,
    finished: AtomicU32,
  }

  impl JobObserver for Counting {
    fn finished(&self, _: &JobContext, outcome: &Outcome, _: Duration) {
      assert_eq!(*outcome, Outcome::Succeeded);
      self.finished.fetch_add(1, Ordering::SeqCst);
    }

    fn store_error(&self, _: &JobError) {
      self.store_errors.fetch_add(1, Ordering::SeqCst);
    }
  }

  #[test]
  fn a_store_outage_is_retried_and_never_ends_the_worker() {
    let s = setup();
    let runs = Arc::new(AtomicU32::new(0));
    let observer = Arc::new(Counting::default());
    let config = WorkerConfig {
      observer: observer.clone(),
      ..s.config()
    };
    s.store.fail_with(Some("down"));
    let h = s.start(Worker::new(registry(&runs)).with_config(config));
    s.exec.advance(Duration::from_secs(5));
    assert!(observer.store_errors.load(Ordering::SeqCst) >= 3);
    assert!(!h.is_done());
    s.store.fail_with(None);
    s.drive(s.store.enqueue(nap(0), JobOptions::new())).unwrap();
    s.exec.advance(Duration::from_secs(31));
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    assert_eq!(observer.finished.load(Ordering::SeqCst), 1);
    h.interrupt_now();
    s.exec.advance(Duration::from_secs(1));
    assert!(h.is_done());
  }

  #[test]
  fn an_enqueue_in_the_same_process_wakes_an_idle_worker() {
    let s = setup();
    let runs = Arc::new(AtomicU32::new(0));
    let config = WorkerConfig {
      poll_interval: Duration::from_secs(3600),
      ..s.config()
    };
    let h = s.start(Worker::new(registry(&runs)).with_config(config));
    s.exec.run_until_stalled();
    s.drive(s.store.enqueue(nap(0), JobOptions::new())).unwrap();
    s.exec.run_until_stalled();
    assert_eq!(runs.load(Ordering::SeqCst), 1, "no poll interval waited");
    assert_eq!(s.only().state, JobState::Succeeded);
    h.interrupt_now();
    s.exec.advance(Duration::from_secs(1));
  }

  #[test]
  fn the_layer_runs_the_worker_for_the_scope_of_the_program() {
    let s = setup();
    let runs = Arc::new(AtomicU32::new(0));
    s.drive(
      s.store
        .enqueue(nap(0), JobOptions::new().priority(Priority::HIGH)),
    )
    .unwrap();
    let worker = Worker::new(registry(&runs)).with_config(s.config());
    let layer = memory_job_queue_layer::<Never>(s.store.clone()).provide_to(worker.layer());
    let program: Effect<String, Never, HCons<WorkerHandle, HNil>> = need::<WorkerHandle>()
      .flat_map(|h| {
        sleep(Duration::from_secs(1)).map(move |()| format!("{} {}", h.id(), h.in_flight()))
      });
    let out = Arc::new(Mutex::new(None));
    let o = Arc::clone(&out);
    let done = program
      .provide_layer(layer)
      .map(move |s| *o.lock().unwrap() = Some(s))
      .fork_root(Arc::new(s.exec.clone()));
    s.exec.advance(Duration::from_secs(2));
    assert!(done.is_done(), "the scope closed and the worker drained");
    assert_eq!(out.lock().unwrap().as_deref(), Some("w1 0"));
    assert_eq!(s.only().state, JobState::Succeeded);
  }

  #[test]
  fn worker_settings_and_ids() {
    let id = WorkerId::generate();
    assert_eq!(id.as_str().split(':').count(), 3, "{id}");
    assert_ne!(WorkerId::generate(), id);
    assert!(format!("{id:?}").starts_with("WorkerId("));
    let c = WorkerConfig::default();
    assert_eq!(
      (c.poll_interval, c.lease, c.rescue_every, c.drain_timeout),
      (
        Duration::from_secs(1),
        Duration::from_secs(300),
        Duration::from_secs(30),
        Duration::from_secs(30)
      )
    );
    assert!(format!("{c:?}").contains("poll_interval"));
    let w = Worker::<JobQueueEnv>::new(JobRegistry::new());
    assert_eq!(w.queues(), [(Queue::default(), 10)]);
    let w = w.queue("mail", 0).queue("mail", 3).queue("sms", 2);
    assert_eq!(
      w.queues(),
      [(Queue::new("mail"), 3), (Queue::new("sms"), 2)]
    );
    assert!(format!("{w:?}").contains("mail"));
    assert_eq!(backoff(0), Duration::from_millis(100));
    assert_eq!(backoff(3), Duration::from_millis(800));
    assert_eq!(backoff(40), MAX_ERROR_BACKOFF);
    let report = DrainReport {
      succeeded: 1,
      retrying: 2,
      dead: 3,
      lost: 4,
    };
    assert_eq!(report.runs(), 10);
    NoopObserver.lost(JobId(1));
  }
}
