//! [`WorkflowEngine`]: registers workflows, starts instances, and drives them under leases,
//! replaying each from its history.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use id_effect::{Effect, Never, Random, UtcDateTime, WallClock, fail, sleep, succeed, suspend};

use crate::WorkflowName;
use crate::error::WorkflowError;
use crate::history::{Claimed, Finish, InstanceRecord, NewInstance, WorkflowStatus, WorkflowStore};
use crate::lease::{EngineId, Fenced, Lease, later};
use crate::workflow::{Workflow, WorkflowContext, WorkflowId, decode, encode};

/// How an engine takes and keeps leases.
#[derive(Clone, Debug)]
pub struct EngineConfig {
  /// The identity it takes leases under (default [`EngineId::random`]). Two running engines
  /// must not share it.
  pub id: EngineId,
  /// How long a lease lasts without renewal (default 30 s). The engine renews it every third of
  /// it while a run is in flight, and every append extends it. A crashed engine's instances are
  /// taken over once it has run out.
  pub lease: Duration,
  /// How long [`WorkflowEngine::run`] waits when nothing is claimable or the store failed
  /// (default 500 ms).
  pub poll_interval: Duration,
}

impl Default for EngineConfig {
  fn default() -> Self {
    Self {
      id: EngineId::random(),
      lease: Duration::from_secs(30),
      poll_interval: Duration::from_millis(500),
    }
  }
}

/// What [`WorkflowEngine::start`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Started {
  /// A new instance was inserted.
  New(WorkflowId),
  /// An instance of the same workflow already had this id; nothing changed.
  Existing(WorkflowId),
}

impl Started {
  /// The instance id.
  pub fn id(&self) -> &WorkflowId {
    match self {
      Started::New(id) | Started::Existing(id) => id,
    }
  }
}

/// How one run of an instance ended.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum RunOutcome {
  /// The body completed; the output is stored and the instance is `completed`.
  Completed,
  /// The instance is `failed` with this error (the body failed, a step failure propagated, the
  /// replay was nondeterministic, or a journaled value no longer decodes).
  Failed(WorkflowError),
  /// The lease was lost mid-run (expired, or taken over): this engine stopped and wrote nothing
  /// more; whoever holds the lease now continues from the history.
  LeaseLost,
}

type Body = Arc<
  dyn Fn(WorkflowContext, serde_json::Value) -> Effect<serde_json::Value, WorkflowError>
    + Send
    + Sync,
>;

/// A registered workflow, erased: its body over encoded values.
struct Registered {
  run: Body,
}

/// Runs durable workflows over a [`WorkflowStore`] (release 0.11.0 decisions D6, D7).
///
/// - [`start`](Self::start) inserts an instance (idempotent per [`WorkflowId`]);
///   [`status`](Self::status), [`instance`](Self::instance) and [`result`](Self::result) read it.
/// - [`run`](Self::run) (or [`run_once`](Self::run_once), [`resume`](Self::resume)) claims
///   instances of the registered workflows whose lease is free or expired and drives each:
///   it loads the history, runs the body with a [`WorkflowContext`] that replays the journaled
///   results, journals every new one under the lease, and finishes the instance.
/// - Any number of engines may run against one store. An instance advances only under its
///   lease, every append is fenced by the lease's token, and a run whose lease is lost stops
///   (see [`crate::lease`]); an expired lease is taken over and the instance resumes from its
///   history on another engine.
///
/// A run that dies (a defect) or is interrupted writes nothing more and keeps its lease until
/// it expires; the instance then resumes, re-running only the step that was in flight.
/// Cloning shares the registry and the store.
#[derive(Clone)]
pub struct WorkflowEngine {
  store: Arc<dyn WorkflowStore>,
  registry: BTreeMap<&'static str, Arc<Registered>>,
  config: EngineConfig,
  clock: WallClock,
  random: Random,
}

impl fmt::Debug for WorkflowEngine {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("WorkflowEngine")
      .field("id", &self.config.id)
      .field("workflows", &self.registry.keys().collect::<Vec<_>>())
      .field("lease", &self.config.lease)
      .finish_non_exhaustive()
  }
}

impl WorkflowEngine {
  /// An engine over `store` with no workflows, the default [`EngineConfig`], the system clock
  /// and [`Random::live`].
  pub fn new(store: impl WorkflowStore) -> Self {
    Self::from_arc(Arc::new(store))
  }

  /// An engine over a shared store.
  pub fn from_arc(store: Arc<dyn WorkflowStore>) -> Self {
    Self {
      store,
      registry: BTreeMap::new(),
      config: EngineConfig::default(),
      clock: WallClock::system(),
      random: Random::live(),
    }
  }

  /// With `config`.
  pub fn with_config(mut self, config: EngineConfig) -> Self {
    self.config = config;
    self
  }

  /// Reads every time (leases, journaled [`WorkflowContext::now`]) from `clock`.
  pub fn with_clock(mut self, clock: WallClock) -> Self {
    self.clock = clock;
    self
  }

  /// Draws [`WorkflowContext::random`] values from `random`.
  pub fn with_random(mut self, random: Random) -> Self {
    self.random = random;
    self
  }

  /// Registers `workflow` under [`Workflow::NAME`]; fails when the name is invalid or taken.
  pub fn register<W: Workflow>(mut self, workflow: W) -> Result<Self, WorkflowError> {
    WorkflowName::new(W::NAME)?;
    if self.registry.contains_key(W::NAME) {
      return Err(WorkflowError::DuplicateWorkflow(W::NAME.to_owned()));
    }
    let workflow = Arc::new(workflow);
    let run: Body = Arc::new(move |ctx, input| {
      let workflow = Arc::clone(&workflow);
      suspend(move || match decode::<W::Input>("input", &input) {
        Ok(input) => workflow.run(ctx, input).map(encode::<W::Output>),
        Err(e) => fail(e),
      })
    });
    self.registry.insert(W::NAME, Arc::new(Registered { run }));
    Ok(self)
  }

  /// The identity it takes leases under.
  pub fn id(&self) -> &EngineId {
    &self.config.id
  }

  /// Its configuration.
  pub fn config(&self) -> &EngineConfig {
    &self.config
  }

  /// The store it runs on.
  pub fn store(&self) -> &Arc<dyn WorkflowStore> {
    &self.store
  }

  /// Starts an instance of `W` with `input` under `id`, recording [`Workflow::VERSION`].
  /// Idempotent: an instance of `W` that already has `id` is left as it is
  /// ([`Started::Existing`]); one of another workflow fails with [`WorkflowError::IdInUse`]. `W`
  /// need not be registered on this engine (a producer may start what other engines run).
  pub fn start<W: Workflow>(
    &self,
    id: WorkflowId,
    input: W::Input,
  ) -> Effect<Started, WorkflowError> {
    if let Err(e) = WorkflowName::new(W::NAME) {
      return fail(e.into());
    }
    let instance = NewInstance {
      id: id.clone(),
      name: W::NAME.to_owned(),
      version: W::VERSION.to_owned(),
      input: encode(input),
    };
    let store = Arc::clone(&self.store);
    self.clock.now().flat_map(move |now| {
      store.create(instance, now).flat_map(move |inserted| {
        if inserted {
          return succeed(Started::New(id));
        }
        store.get(&id).flat_map(move |found| match found {
          Some(record) if record.name == W::NAME => succeed(Started::Existing(id)),
          Some(record) => fail(WorkflowError::IdInUse {
            id: id.to_string(),
            workflow: record.name,
          }),
          // Inserted by a concurrent start and not visible yet cannot happen (the insert
          // conflicted on a committed row); a deleted row can.
          None => fail(WorkflowError::NotFound(id.to_string())),
        })
      })
    })
  }

  /// The instance `id` as stored, if any.
  pub fn instance(&self, id: &WorkflowId) -> Effect<Option<InstanceRecord>, WorkflowError> {
    self.store.get(id)
  }

  /// The status of instance `id` ([`WorkflowError::NotFound`] when there is none).
  pub fn status(&self, id: &WorkflowId) -> Effect<WorkflowStatus, WorkflowError> {
    let missing = id.to_string();
    self.store.get(id).flat_map(move |found| match found {
      Some(record) => succeed(record.status),
      None => fail(WorkflowError::NotFound(missing)),
    })
  }

  /// The output of instance `id` of `W`: `None` while it runs, the decoded output once completed,
  /// [`WorkflowError::Failed`] with the recorded error once failed, [`WorkflowError::NotFound`]
  /// when there is no such instance and [`WorkflowError::IdInUse`] when it is another workflow.
  pub fn result<W: Workflow>(&self, id: &WorkflowId) -> Effect<Option<W::Output>, WorkflowError> {
    let missing = id.to_string();
    self.store.get(id).flat_map(move |found| {
      let record = match found {
        None => return fail(WorkflowError::NotFound(missing)),
        Some(record) if record.name != W::NAME => {
          return fail(WorkflowError::IdInUse {
            id: missing,
            workflow: record.name,
          });
        }
        Some(record) => record,
      };
      match record.status {
        WorkflowStatus::Completed => match record.output.as_ref() {
          Some(output) => match decode::<W::Output>("output", output) {
            Ok(output) => succeed(Some(output)),
            Err(e) => fail(e),
          },
          None => fail(WorkflowError::Decode {
            what: "output".into(),
            message: "a completed instance has no output".into(),
          }),
        },
        WorkflowStatus::Failed => fail(WorkflowError::Failed(
          record.error.unwrap_or_else(|| "unknown error".into()),
        )),
        _ => succeed(None),
      }
    })
  }

  /// Takes the lease of instance `id` when it is running and its lease is free or expired, and
  /// drives it; `None` when it could not be leased (held by a live lease, finished, or absent).
  pub fn resume(&self, id: &WorkflowId) -> Effect<Option<RunOutcome>, WorkflowError> {
    let engine = self.clone();
    let id = id.clone();
    self.leases().flat_map(move |(now, until)| {
      engine
        .store
        .acquire(&id, &engine.config.id, now, until)
        .flat_map(move |claimed| match claimed {
          None => succeed(None),
          Some(claimed) => engine.drive(claimed).map(Some),
        })
    })
  }

  /// Claims and drives, one at a time, every instance of a registered workflow whose lease is
  /// free or expired, until none is left; the number of runs.
  pub fn run_once(&self) -> Effect<usize, WorkflowError> {
    let names: Vec<&'static str> = self.registry.keys().copied().collect();
    self.drain(names, 0)
  }

  fn drain(&self, mut names: Vec<&'static str>, done: usize) -> Effect<usize, WorkflowError> {
    let Some(&name) = names.first() else {
      return succeed(done);
    };
    let engine = self.clone();
    self.leases().flat_map(move |(now, until)| {
      engine
        .store
        .claim(name, &engine.config.id, 1, now, until)
        .flat_map(move |claimed| match claimed.into_iter().next() {
          None => {
            names.remove(0);
            engine.drain(names, done)
          }
          Some(claimed) => {
            let next = engine.clone();
            engine
              .drive(claimed)
              .flat_map(move |_| next.drain(names, done + 1))
          }
        })
    })
  }

  /// [`run_once`](Self::run_once) forever, waiting [`EngineConfig::poll_interval`] whenever
  /// nothing was claimable or the store failed (a store error is retried, not returned). Fork
  /// it, and fork it more than once to drive several instances at a time; interrupt it to stop.
  pub fn run(&self) -> Effect<Never, WorkflowError> {
    let engine = self.clone();
    let poll = self.config.poll_interval;
    suspend(move || {
      let next = engine.clone();
      engine
        .run_once()
        .catch(|_| succeed(0))
        .flat_map(move |runs| {
          let wait = if runs == 0 { sleep(poll) } else { succeed(()) };
          wait.flat_map(move |()| next.run())
        })
    })
  }

  /// The clock's now and the expiry of a lease taken now.
  fn leases(&self) -> Effect<(UtcDateTime, UtcDateTime), WorkflowError> {
    let lease = self.config.lease;
    self.clock.now().map(move |now| (now, later(now, lease)))
  }

  /// Runs one claimed instance to its end or until its lease is lost.
  fn drive(&self, claimed: Claimed) -> Effect<RunOutcome, WorkflowError> {
    let Claimed {
      lease, name, input, ..
    } = claimed;
    let Some(registered) = self.registry.get(name.as_str()).cloned() else {
      // `resume` of an instance this engine cannot run: give the lease back.
      return self
        .release(lease)
        .flat_map(move |()| fail(WorkflowError::UnknownWorkflow(name)));
    };
    let engine = self.clone();
    let store = Arc::clone(&self.store);
    store.history(&lease.instance).flat_map(move |history| {
      let ctx = WorkflowContext::new(
        lease.clone(),
        history,
        Arc::clone(&engine.store),
        engine.clock.clone(),
        engine.random.clone(),
        engine.config.lease,
      );
      let check = ctx.clone();
      let body = (registered.run)(ctx, input)
        .flat_map(move |output| match check.leftover() {
          Some(e) => fail(e),
          None => succeed(output),
        })
        .map(Ok)
        .catch(|e| succeed(Err(e)));
      body
        .race_first(engine.heartbeat(lease.clone()))
        .flat_map(move |result| engine.settle(lease, result))
    })
  }

  /// Renews `lease` every third of the lease until a renewal is rejected; then the run's result
  /// is [`WorkflowError::LeaseLost`]. A store error is retried at the next beat (the run's
  /// appends fail on their own when the store is down).
  fn heartbeat(
    &self,
    lease: Lease,
  ) -> Effect<Result<serde_json::Value, WorkflowError>, WorkflowError> {
    let every = (self.config.lease / 3).max(Duration::from_millis(1));
    let (store, clock, length) = (
      Arc::clone(&self.store),
      self.clock.clone(),
      self.config.lease,
    );
    let engine = self.clone();
    sleep(every).then(clock.now()).flat_map(move |now| {
      store
        .renew(&lease, now, later(now, length))
        .catch(|_| succeed(Fenced::Applied))
        .flat_map(move |fenced| match fenced {
          Fenced::Applied => engine.heartbeat(lease),
          Fenced::Lost => succeed(Err(WorkflowError::LeaseLost)),
        })
    })
  }

  /// Finishes the instance from the run's result.
  fn settle(
    &self,
    lease: Lease,
    result: Result<serde_json::Value, WorkflowError>,
  ) -> Effect<RunOutcome, WorkflowError> {
    let (finish, failure) = match result {
      Ok(output) => (Finish::Completed(output), None),
      Err(WorkflowError::LeaseLost) => return succeed(RunOutcome::LeaseLost),
      // The store failed mid-run: nothing is decided about the instance; free the lease so it
      // resumes as soon as the store is back.
      Err(e @ WorkflowError::Sql(_)) => {
        return self.release(lease).flat_map(move |()| fail(e));
      }
      Err(e) => (Finish::Failed(e.to_string()), Some(e)),
    };
    let store = Arc::clone(&self.store);
    self.clock.now().flat_map(move |now| {
      store
        .finish(&lease, finish, now)
        .map(move |fenced| match (fenced, failure) {
          (Fenced::Lost, _) => RunOutcome::LeaseLost,
          (Fenced::Applied, None) => RunOutcome::Completed,
          (Fenced::Applied, Some(e)) => RunOutcome::Failed(e),
        })
    })
  }

  /// Frees `lease`, ignoring the outcome (best effort; an unreleased lease expires).
  fn release(&self, lease: Lease) -> Effect<(), WorkflowError> {
    let store = Arc::clone(&self.store);
    self.clock.now().flat_map(move |now| {
      store
        .release(&lease, now)
        .map(|_| ())
        .catch(|_| succeed(()))
    })
  }
}
