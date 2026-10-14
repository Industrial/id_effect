//! [`EffectExecutor`] — the crate's single strategy for running `!Send` effects behind the `Send`
//! futures Axum and Tower require.
//!
//! ## Why
//!
//! [`id_effect::Effect`] futures are intentionally **not** [`Send`] (they box `!Send` futures so
//! effects can hold `Rc`, `RefCell`, fiber-local state, …). Axum handlers and most Tower stacks need
//! [`Send`] futures. Earlier releases bridged the gap with `block_in_place` + `Handle::block_on`,
//! which parks a runtime worker for the whole request and panics on `current_thread` runtimes.
//!
//! ## How
//!
//! An [`EffectExecutor`] is an **elastic pool of dedicated OS threads** (not Tokio's blocking pool).
//! Every in-flight request gets a thread of its own, running a private `current_thread` Tokio runtime
//! (I/O + time enabled). The caller ships a [`Send`] *recipe* (`env` + the closure that builds the
//! effect); the worker builds the effect, drives it with [`id_effect::run_async`] inside a
//! [`LocalSet`](tokio::task::LocalSet), and sends the result back over a oneshot channel. The
//! caller's future is therefore [`Send`], never blocks a thread, and works on any runtime flavour.
//!
//! - **Isolation.** `Effect::new` runs synchronous closures; one effect doing `std::fs`, a CPU loop
//!   or `thread::sleep` occupies only its own request's thread, never other requests.
//! - **Elasticity.** Threads are spawned on demand up to [`EffectExecutorBuilder::max_threads`]
//!   (default 512), cached after a request finishes and retired after
//!   [`EffectExecutorBuilder::idle_timeout`] (default 10 s) without work.
//! - **Backpressure.** At most `max_threads` requests run and at most
//!   [`EffectExecutorBuilder::queue_capacity`] (default 1024) wait for a thread. Beyond that,
//!   [`SaturationPolicy::Wait`] (default) makes callers await admission, while
//!   [`SaturationPolicy::Reject`] makes the `try_*` entry points return [`Saturated`] — which the
//!   routing helpers turn into `503 Service Unavailable`.
//! - **Cancellation.** By default ([`DropPolicy::RunToCompletion`]) an effect always runs to the
//!   end even if the caller's future is dropped (client disconnect): the result is discarded, but no
//!   effect stops half-way between two side effects. [`DropPolicy::Cancel`] opts into dropping the
//!   effect at its next `.await` instead (synchronous steps cannot be interrupted).
//! - **Panics** inside the effect are re-raised in the caller, like an inline `.await`.
//!
//! Inside an effect, [`tokio::spawn`] / [`tokio::runtime::Handle::current`] refer to the worker
//! thread's runtime: spawned tasks progress while that thread is alive (running a request or idle)
//! and are dropped when it retires; `spawn_local` tasks end with their request. Spawn long-lived
//! background work on your application runtime instead. Tokio resources created on the application
//! runtime (pools, sockets) keep working — their driver keeps running there.

use std::collections::VecDeque;
use std::fmt;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::task::Poll;
use std::time::Duration;

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use id_effect::{Effect, run_async};
use tokio::runtime::Runtime;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};

type Job = Box<dyn FnOnce(&Runtime) + Send + 'static>;

static GLOBAL: OnceLock<EffectExecutor> = OnceLock::new();

/// What happens to a running effect when the caller's future is dropped.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DropPolicy {
  /// The effect keeps running to completion; its result is discarded (default).
  #[default]
  RunToCompletion,
  /// The effect is dropped at its next `.await` point.
  Cancel,
}

/// What the `try_*` entry points do when every thread is busy and the queue is full.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SaturationPolicy {
  /// Wait for admission (default). The non-`try` entry points always wait.
  #[default]
  Wait,
  /// Fail fast with [`Saturated`] (mapped to `503` by the routing helpers).
  Reject,
}

/// The executor is saturated and its [`SaturationPolicy`] is [`SaturationPolicy::Reject`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Saturated;

impl fmt::Display for Saturated {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("effect executor saturated")
  }
}

impl std::error::Error for Saturated {}

impl IntoResponse for Saturated {
  /// `503 Service Unavailable` with `Retry-After: 1`.
  fn into_response(self) -> Response {
    let mut res = (StatusCode::SERVICE_UNAVAILABLE, "effect executor saturated").into_response();
    res
      .headers_mut()
      .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
    res
  }
}

/// Configuration for an [`EffectExecutor`]; start from [`EffectExecutor::builder`].
#[derive(Clone, Debug)]
pub struct EffectExecutorBuilder {
  max_threads: usize,
  queue_capacity: usize,
  idle_timeout: Duration,
  drop_policy: DropPolicy,
  saturation: SaturationPolicy,
  /// Test hook: receives a worker's thread id once it has retired.
  #[cfg(test)]
  retired: Option<std::sync::mpsc::Sender<std::thread::ThreadId>>,
}

impl Default for EffectExecutorBuilder {
  fn default() -> Self {
    Self {
      max_threads: 512,
      queue_capacity: 1024,
      idle_timeout: Duration::from_secs(10),
      drop_policy: DropPolicy::RunToCompletion,
      saturation: SaturationPolicy::Wait,
      #[cfg(test)]
      retired: None,
    }
  }
}

impl EffectExecutorBuilder {
  /// Maximum number of concurrently running requests (= worker threads); at least 1.
  #[must_use]
  pub fn max_threads(mut self, n: usize) -> Self {
    self.max_threads = n.max(1);
    self
  }

  /// Requests allowed to wait for a free thread before the executor counts as saturated.
  #[must_use]
  pub fn queue_capacity(mut self, n: usize) -> Self {
    self.queue_capacity = n;
    self
  }

  /// How long a cached worker thread waits for new work before it exits.
  #[must_use]
  pub fn idle_timeout(mut self, d: Duration) -> Self {
    self.idle_timeout = d;
    self
  }

  /// See [`DropPolicy`].
  #[must_use]
  pub fn drop_policy(mut self, p: DropPolicy) -> Self {
    self.drop_policy = p;
    self
  }

  /// See [`SaturationPolicy`].
  #[must_use]
  pub fn saturation(mut self, p: SaturationPolicy) -> Self {
    self.saturation = p;
    self
  }

  #[cfg(test)]
  fn on_retire(mut self, tx: std::sync::mpsc::Sender<std::thread::ThreadId>) -> Self {
    self.retired = Some(tx);
    self
  }

  /// Build the executor. No thread is started until the first request.
  pub fn build(self) -> EffectExecutor {
    let admission = Arc::new(Semaphore::new(
      self
        .max_threads
        .saturating_add(self.queue_capacity)
        .min(Semaphore::MAX_PERMITS),
    ));
    EffectExecutor {
      handle: Arc::new(Handle {
        shared: Arc::new(Shared {
          config: self,
          admission,
          state: Mutex::new(PoolState::default()),
          next_id: AtomicU64::new(0),
        }),
      }),
    }
  }
}

struct Shared {
  config: EffectExecutorBuilder,
  admission: Arc<Semaphore>,
  state: Mutex<PoolState>,
  next_id: AtomicU64,
}

impl Shared {
  fn lock(&self) -> MutexGuard<'_, PoolState> {
    self.state.lock().unwrap_or_else(|e| e.into_inner())
  }
}

#[derive(Default)]
struct PoolState {
  /// Admitted jobs waiting for a thread (bounded by the admission semaphore).
  queue: VecDeque<Job>,
  /// Cached idle workers, each waiting on its own channel.
  idle: Vec<(u64, mpsc::UnboundedSender<Job>)>,
  threads: usize,
  shutdown: bool,
}

/// Last-clone guard: shuts the pool down (idle workers exit, busy ones finish and drain the queue).
struct Handle {
  shared: Arc<Shared>,
}

impl Drop for Handle {
  fn drop(&mut self) {
    let mut st = self.shared.lock();
    st.shutdown = true;
    st.idle.clear();
  }
}

/// Elastic thread-per-request pool that runs `!Send` effects for `Send` callers.
///
/// Cheap to clone (clones share the pool). Most code uses the lazily created
/// [`EffectExecutor::global`] implicitly through [`crate::routing`], [`crate::execute`],
/// [`crate::run_with_env`] and the `service` module; call [`EffectExecutor::install_global`] early in
/// `main` to configure it.
#[derive(Clone)]
pub struct EffectExecutor {
  handle: Arc<Handle>,
}

impl Default for EffectExecutor {
  fn default() -> Self {
    Self::new()
  }
}

impl EffectExecutor {
  /// Executor with the default configuration (see [`EffectExecutorBuilder`]).
  pub fn new() -> Self {
    Self::builder().build()
  }

  /// Start configuring an executor.
  pub fn builder() -> EffectExecutorBuilder {
    EffectExecutorBuilder::default()
  }

  /// The process-wide executor (default configuration unless [`Self::install_global`] ran first).
  pub fn global() -> &'static EffectExecutor {
    GLOBAL.get_or_init(EffectExecutor::new)
  }

  /// Install `executor` as [`EffectExecutor::global`].
  ///
  /// Returns the executor back if the global one was already created (installed or used).
  pub fn install_global(executor: EffectExecutor) -> Result<(), EffectExecutor> {
    GLOBAL.set(executor)
  }

  /// Number of live worker threads (busy or idle).
  pub fn threads(&self) -> usize {
    self.shared().lock().threads
  }

  /// The configured [`DropPolicy`].
  pub fn drop_policy(&self) -> DropPolicy {
    self.shared().config.drop_policy
  }

  /// The configured [`SaturationPolicy`].
  pub fn saturation(&self) -> SaturationPolicy {
    self.shared().config.saturation
  }

  /// Build `build(&mut env)` on a worker and drive it to completion with the same `env`.
  ///
  /// Always waits for admission when saturated (use [`Self::try_run`] to honour
  /// [`SaturationPolicy::Reject`]).
  pub async fn run<S, A, E, F>(&self, env: S, build: F) -> Result<A, E>
  where
    S: Send + 'static,
    A: Send + 'static,
    E: Send + 'static,
    F: FnOnce(&mut S) -> Effect<A, E, S> + Send + 'static,
  {
    self.run_local(move || drive(env, build)).await
  }

  /// Like [`Self::run`], but returns [`Saturated`] instead of waiting when the executor is full and
  /// configured with [`SaturationPolicy::Reject`].
  pub async fn try_run<S, A, E, F>(&self, env: S, build: F) -> Result<Result<A, E>, Saturated>
  where
    S: Send + 'static,
    A: Send + 'static,
    E: Send + 'static,
    F: FnOnce(&mut S) -> Effect<A, E, S> + Send + 'static,
  {
    self.try_run_local(move || drive(env, build)).await
  }

  /// Run the `!Send` future produced by `make` on a worker and return its output.
  ///
  /// Lower-level than [`Self::run`]; use it when one request needs several effect runs (e.g. the
  /// in-flight bookkeeping of the `service` module's `EffectService`). Waits when saturated.
  pub async fn run_local<T, Fut, M>(&self, make: M) -> T
  where
    T: Send + 'static,
    Fut: Future<Output = T> + 'static,
    M: FnOnce() -> Fut + Send + 'static,
  {
    let permit = self
      .shared()
      .admission
      .clone()
      .acquire_owned()
      .await
      .expect("effect executor admission semaphore is never closed");
    self.submit(permit, make).await
  }

  /// [`Self::run_local`] honouring [`SaturationPolicy::Reject`].
  pub async fn try_run_local<T, Fut, M>(&self, make: M) -> Result<T, Saturated>
  where
    T: Send + 'static,
    Fut: Future<Output = T> + 'static,
    M: FnOnce() -> Fut + Send + 'static,
  {
    let shared = self.shared();
    let permit = match shared.config.saturation {
      SaturationPolicy::Wait => shared
        .admission
        .clone()
        .acquire_owned()
        .await
        .expect("effect executor admission semaphore is never closed"),
      SaturationPolicy::Reject => shared
        .admission
        .clone()
        .try_acquire_owned()
        .map_err(|_| Saturated)?,
    };
    Ok(self.submit(permit, make).await)
  }

  fn shared(&self) -> &Arc<Shared> {
    &self.handle.shared
  }

  async fn submit<T, Fut, M>(&self, permit: OwnedSemaphorePermit, make: M) -> T
  where
    T: Send + 'static,
    Fut: Future<Output = T> + 'static,
    M: FnOnce() -> Fut + Send + 'static,
  {
    let (tx, rx) = oneshot::channel::<std::thread::Result<T>>();
    let cancel = self.drop_policy() == DropPolicy::Cancel;
    self.dispatch(Box::new(move |rt: &Runtime| {
      let mut tx = tx;
      let tx_ref = &mut tx;
      let outcome = std::panic::catch_unwind(AssertUnwindSafe(move || {
        let local = tokio::task::LocalSet::new();
        local.block_on(rt, async move {
          let fut = make();
          if cancel {
            until_caller_drops(fut, tx_ref).await
          } else {
            Some(fut.await)
          }
        })
      }));
      // Admission is released once the effect has really finished — and before the caller can
      // observe the result, so a follow-up request is never rejected by this one's permit.
      drop(permit);
      match outcome {
        Ok(Some(out)) => {
          let _ = tx.send(Ok(out));
        }
        Ok(None) => {} // cancelled: the caller is gone
        Err(payload) => {
          let _ = tx.send(Err(payload));
        }
      }
    }));
    match rx.await {
      Ok(Ok(out)) => out,
      Ok(Err(payload)) => std::panic::resume_unwind(payload),
      Err(_) => {
        panic!("id_effect_axum: effect executor worker stopped before the effect completed")
      }
    }
  }

  fn dispatch(&self, mut job: Job) {
    let shared = self.shared();
    let mut st = shared.lock();
    while let Some((_, tx)) = st.idle.pop() {
      match tx.send(job) {
        Ok(()) => return,
        Err(mpsc::error::SendError(back)) => job = back,
      }
    }
    if st.threads < shared.config.max_threads {
      st.threads += 1;
      drop(st);
      let worker_shared = Arc::clone(shared);
      let id = shared.next_id.fetch_add(1, Ordering::Relaxed);
      let spawned = std::thread::Builder::new()
        .name(format!("id-effect-axum-{id}"))
        .spawn(move || worker(worker_shared, job));
      if spawned.is_err() {
        // The job is dropped with the closure; its caller observes a stopped worker.
        shared.lock().threads -= 1;
      }
    } else {
      st.queue.push_back(job);
    }
  }
}

impl fmt::Debug for EffectExecutor {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let shared = self.shared();
    f.debug_struct("EffectExecutor")
      .field("threads", &self.threads())
      .field("config", &shared.config)
      .finish()
  }
}

async fn drive<S, A, E, F>(mut env: S, build: F) -> Result<A, E>
where
  S: 'static,
  A: 'static,
  E: 'static,
  F: FnOnce(&mut S) -> Effect<A, E, S>,
{
  let eff = build(&mut env);
  run_async(eff, env).await
}

/// Poll `fut` until it completes (`Some`) or the caller hangs up (`None`, `fut` is dropped).
async fn until_caller_drops<T, F: Future<Output = T>>(
  fut: F,
  tx: &mut oneshot::Sender<std::thread::Result<T>>,
) -> Option<T> {
  let mut fut = std::pin::pin!(fut);
  std::future::poll_fn(|cx| {
    if let Poll::Ready(out) = fut.as_mut().poll(cx) {
      return Poll::Ready(Some(out));
    }
    if tx.poll_closed(cx).is_ready() {
      return Poll::Ready(None);
    }
    Poll::Pending
  })
  .await
}

enum Next {
  Run(Job),
  Wait(u64, mpsc::UnboundedReceiver<Job>),
}

fn retire(shared: &Shared) {
  shared.lock().threads -= 1;
}

fn worker(shared: Arc<Shared>, first: Job) {
  worker_loop(&shared, first);
  // `worker_loop` has already removed this thread from the pool's count.
  #[cfg(test)]
  if let Some(tx) = &shared.config.retired {
    let _ = tx.send(std::thread::current().id());
  }
}

fn worker_loop(shared: &Shared, first: Job) {
  let rt = match tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()
  {
    Ok(rt) => rt,
    Err(_) => return retire(shared), // `first` is dropped; its caller sees a stopped worker
  };
  let mut job = first;
  loop {
    job(&rt);
    let next = {
      let mut st = shared.lock();
      if let Some(queued) = st.queue.pop_front() {
        Next::Run(queued)
      } else if st.shutdown {
        st.threads -= 1;
        return;
      } else {
        let id = shared.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::unbounded_channel();
        st.idle.push((id, tx));
        Next::Wait(id, rx)
      }
    };
    job = match next {
      Next::Run(queued) => queued,
      Next::Wait(id, mut rx) => {
        // Idle inside `block_on` so tasks spawned on this runtime keep making progress.
        let idle_timeout = shared.config.idle_timeout;
        match rt.block_on(async { tokio::time::timeout(idle_timeout, rx.recv()).await }) {
          Ok(Some(next)) => next,
          Ok(None) => return retire(shared), // executor dropped
          Err(_) => {
            let mut st = shared.lock();
            if let Some(pos) = st.idle.iter().position(|(i, _)| *i == id) {
              st.idle.swap_remove(pos);
              st.threads -= 1;
              return;
            }
            drop(st);
            // A dispatcher claimed this worker just as it timed out: take that job.
            match rt.block_on(rx.recv()) {
              Some(next) => next,
              None => return retire(shared),
            }
          }
        }
      }
    };
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect::{fail, succeed};
  use std::rc::Rc;
  use std::sync::atomic::AtomicBool;

  fn assert_send<T: Send>(_: &T) {}

  /// Fires its sender when dropped (proves an effect future was dropped).
  struct DropSignal(Option<oneshot::Sender<()>>);

  impl Drop for DropSignal {
    fn drop(&mut self) {
      if let Some(tx) = self.0.take() {
        let _ = tx.send(());
      }
    }
  }

  const PATIENCE: Duration = Duration::from_secs(10);

  #[tokio::test(flavor = "current_thread")]
  async fn run_works_on_current_thread_runtime() {
    let exec = EffectExecutor::new();
    let out = exec
      .run(5_u32, |n: &mut u32| succeed::<_, (), _>(*n * 2))
      .await;
    assert_eq!(out, Ok(10));
    let err = exec.run((), |_: &mut ()| fail::<u8, _, _>("nope")).await;
    assert_eq!(err, Err("nope"));
  }

  #[tokio::test(flavor = "current_thread")]
  async fn run_future_is_send_and_supports_non_send_effect_state() {
    let fut = EffectExecutor::global().run((), |_: &mut ()| {
      // `Rc` makes the effect body `!Send`; only the recipe crosses threads.
      let rc = Rc::new(3_u8);
      Effect::new_async(move |_r: &mut ()| {
        id_effect::box_future(async move {
          tokio::time::sleep(Duration::from_millis(1)).await;
          Ok::<u8, ()>(*rc)
        })
      })
    });
    assert_send(&fut);
    assert_eq!(fut.await, Ok(3));
  }

  #[tokio::test(flavor = "current_thread")]
  async fn caller_thread_is_never_blocked() {
    // The effect waits on a flag the caller's (single) thread sets afterwards: with
    // `block_in_place` this would deadlock or panic.
    let exec = EffectExecutor::new();
    let flag = Arc::new(AtomicBool::new(false));
    let f2 = flag.clone();
    let run = exec.run((), move |_: &mut ()| {
      Effect::new_async(move |_r: &mut ()| {
        id_effect::box_future(async move {
          while !f2.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(1)).await;
          }
          Ok::<(), ()>(())
        })
      })
    });
    let setter = async {
      tokio::time::sleep(Duration::from_millis(5)).await;
      flag.store(true, Ordering::SeqCst);
    };
    let (out, ()) = tokio::join!(run, setter);
    assert_eq!(out, Ok(()));
  }

  #[tokio::test(flavor = "current_thread")]
  async fn blocking_effect_only_occupies_its_own_request() {
    let exec = EffectExecutor::builder().max_threads(4).build();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let (started_tx, started_rx) = oneshot::channel::<()>();
    let e2 = exec.clone();
    let blocked = tokio::spawn(async move {
      e2.run((), move |_: &mut ()| {
        // A synchronous, thread-blocking step (as `Effect::new` closures often are).
        Effect::new(move |_r: &mut ()| {
          let _ = started_tx.send(());
          release_rx.recv().expect("release");
          Ok::<u8, ()>(1)
        })
      })
      .await
    });
    started_rx.await.expect("blocked effect started");

    let other = tokio::time::timeout(PATIENCE, exec.run((), |_: &mut ()| succeed::<_, (), _>(2)))
      .await
      .expect("second request must not wait behind the blocked one");
    assert_eq!(other, Ok(2));

    release_tx.send(()).unwrap();
    assert_eq!(blocked.await.unwrap(), Ok(1));
  }

  #[tokio::test(flavor = "current_thread")]
  async fn default_policy_runs_effect_to_completion_after_caller_drops() {
    let exec = EffectExecutor::new();
    assert_eq!(exec.drop_policy(), DropPolicy::RunToCompletion);
    let (started_tx, started_rx) = oneshot::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let (finished_tx, finished_rx) = oneshot::channel::<()>();
    let e2 = exec.clone();
    let caller = tokio::spawn(async move {
      e2.run((), move |_: &mut ()| {
        Effect::new(move |_r: &mut ()| {
          let _ = started_tx.send(());
          release_rx.recv().expect("release");
          let _ = finished_tx.send(()); // the "credit" after the "debit"
          Ok::<(), ()>(())
        })
      })
      .await
    });
    started_rx.await.unwrap();
    caller.abort(); // client disconnect
    assert!(caller.await.unwrap_err().is_cancelled());
    release_tx.send(()).unwrap();
    tokio::time::timeout(PATIENCE, finished_rx)
      .await
      .expect("effect must finish although its caller is gone")
      .unwrap();
  }

  #[tokio::test(flavor = "current_thread")]
  async fn cancel_policy_drops_effect_when_caller_drops() {
    let exec = EffectExecutor::builder()
      .drop_policy(DropPolicy::Cancel)
      .build();
    let (started_tx, started_rx) = oneshot::channel::<()>();
    let (dropped_tx, dropped_rx) = oneshot::channel::<()>();
    let finished = Arc::new(AtomicBool::new(false));
    let f2 = finished.clone();
    let e2 = exec.clone();
    let caller = tokio::spawn(async move {
      e2.run((), move |_: &mut ()| {
        Effect::new_async(move |_r: &mut ()| {
          id_effect::box_future(async move {
            let _guard = DropSignal(Some(dropped_tx));
            let _ = started_tx.send(());
            tokio::time::sleep(Duration::from_secs(3600)).await;
            f2.store(true, Ordering::SeqCst);
            Ok::<(), ()>(())
          })
        })
      })
      .await
    });
    started_rx.await.unwrap();
    caller.abort();
    tokio::time::timeout(PATIENCE, dropped_rx)
      .await
      .expect("effect must be dropped once the caller is gone")
      .unwrap();
    assert!(!finished.load(Ordering::SeqCst));
  }

  #[tokio::test(flavor = "current_thread")]
  async fn reject_policy_returns_saturated_then_recovers() {
    let exec = EffectExecutor::builder()
      .max_threads(1)
      .queue_capacity(0)
      .saturation(SaturationPolicy::Reject)
      .build();
    let (started_tx, started_rx) = oneshot::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let e2 = exec.clone();
    let busy = tokio::spawn(async move {
      e2.try_run((), move |_: &mut ()| {
        let _ = started_tx.send(());
        release_rx.recv().expect("release");
        succeed::<_, (), _>(1_u8)
      })
      .await
    });
    started_rx.await.unwrap();
    let rejected = exec
      .try_run((), |_: &mut ()| succeed::<_, (), _>(2_u8))
      .await;
    assert_eq!(rejected, Err(Saturated));
    release_tx.send(()).unwrap();
    assert_eq!(busy.await.unwrap(), Ok(Ok(1)));
    let ok = exec
      .try_run((), |_: &mut ()| succeed::<_, (), _>(3_u8))
      .await;
    assert_eq!(ok, Ok(Ok(3)));
  }

  #[tokio::test(flavor = "current_thread")]
  async fn wait_policy_queues_until_a_thread_frees_up() {
    let exec = EffectExecutor::builder()
      .max_threads(1)
      .queue_capacity(0)
      .build();
    let (started_tx, started_rx) = oneshot::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let e2 = exec.clone();
    let busy = tokio::spawn(async move {
      e2.run((), move |_: &mut ()| {
        let _ = started_tx.send(());
        release_rx.recv().expect("release");
        succeed::<_, (), _>(1_u8)
      })
      .await
    });
    started_rx.await.unwrap();
    let e3 = exec.clone();
    let mut waiting =
      tokio::spawn(async move { e3.try_run((), |_: &mut ()| succeed::<_, (), _>(2_u8)).await });
    assert!(
      tokio::time::timeout(Duration::from_millis(20), &mut waiting)
        .await
        .is_err(),
      "second request waits while the only thread is busy"
    );
    release_tx.send(()).unwrap();
    assert_eq!(busy.await.unwrap(), Ok(1));
    assert_eq!(waiting.await.unwrap(), Ok(Ok(2)));
  }

  /// Thread id of the worker that built the effect.
  async fn run_on_thread(exec: &EffectExecutor) -> std::thread::ThreadId {
    exec
      .run((), |_: &mut ()| {
        succeed::<_, (), _>(std::thread::current().id())
      })
      .await
      .unwrap()
  }

  #[tokio::test(flavor = "current_thread")]
  async fn sequential_requests_reuse_the_cached_thread() {
    // One thread, effectively no idle expiry: every request must land on the same thread,
    // whatever the scheduling (a busy worker means the request queues for it).
    let exec = EffectExecutor::builder()
      .max_threads(1)
      .idle_timeout(Duration::from_secs(24 * 3600))
      .build();
    let first = run_on_thread(&exec).await;
    for _ in 0..10 {
      assert_eq!(run_on_thread(&exec).await, first);
    }
    assert_eq!(exec.threads(), 1);
    assert_ne!(first, std::thread::current().id());
  }

  #[tokio::test(flavor = "current_thread")]
  async fn idle_thread_retires_and_a_fresh_one_serves_the_next_request() {
    let (retired_tx, retired_rx) = std::sync::mpsc::channel();
    let exec = EffectExecutor::builder()
      .max_threads(1)
      .idle_timeout(Duration::ZERO)
      .on_retire(retired_tx)
      .build();
    let first = run_on_thread(&exec).await;
    // Block (off the runtime thread) until the hook reports the retirement — no deadline guess.
    let retired = tokio::task::spawn_blocking(move || {
      let id = retired_rx.recv().expect("retire signal");
      (id, retired_rx)
    })
    .await
    .unwrap();
    let (retired_id, retired_rx) = retired;
    assert_eq!(retired_id, first);
    assert_eq!(
      exec.threads(),
      0,
      "the hook fires after the pool count drops"
    );

    let second = run_on_thread(&exec).await;
    assert_ne!(
      second, first,
      "thread ids are never reused within a process"
    );
    drop(exec);
    // Dropping the executor retires the remaining idle worker as well.
    let last = tokio::task::spawn_blocking(move || retired_rx.recv().expect("retire on drop"))
      .await
      .unwrap();
    assert_eq!(last, second);
  }

  #[tokio::test(flavor = "current_thread")]
  #[should_panic(expected = "boom")]
  async fn effect_panic_is_resumed_in_caller() {
    let exec = EffectExecutor::new();
    let _ = exec
      .run((), |_: &mut ()| -> Effect<(), (), ()> { panic!("boom") })
      .await;
  }

  #[tokio::test(flavor = "current_thread")]
  async fn worker_survives_a_panicking_request() {
    let exec = EffectExecutor::builder().max_threads(1).build();
    let e2 = exec.clone();
    let panicked = tokio::spawn(async move {
      e2.run((), |_: &mut ()| -> Effect<(), (), ()> { panic!("boom") })
        .await
    })
    .await;
    assert!(panicked.unwrap_err().is_panic());
    assert_eq!(
      exec.run((), |_: &mut ()| succeed::<_, (), _>(7)).await,
      Ok(7)
    );
  }

  #[test]
  fn builder_clamps_debug_and_saturated_response() {
    let exec = EffectExecutor::builder().max_threads(0).build();
    let rendered = format!("{exec:?}");
    assert!(rendered.contains("max_threads: 1"), "{rendered}");
    assert_eq!(exec.saturation(), SaturationPolicy::Wait);
    let res = Saturated.into_response();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(res.headers()[header::RETRY_AFTER], "1");
    assert_eq!(Saturated.to_string(), "effect executor saturated");
    let _ = EffectExecutor::global();
    assert!(EffectExecutor::install_global(EffectExecutor::default()).is_err());
  }
}
