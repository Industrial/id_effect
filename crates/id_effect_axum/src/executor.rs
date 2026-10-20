//! [`EffectExecutor`]: runs one [`id_effect::Effect`] per request as a fiber on the
//! [`TokioExecutor`] of the runtime that serves the request.
//!
//! ## How
//!
//! Effects are [`Send`], so they run where Axum already runs: every request's effect is a root
//! fiber on the ambient tokio runtime (a [`TokioExecutor`] over
//! [`tokio::runtime::Handle::current`]), driven by the handler future itself until its [`Exit`]
//! (since 0.11; before, each fiber was spawned as its own task). An effect that completes without
//! suspending (a middleware check, most handlers) therefore costs no task spawn and no
//! cross-thread wake-up, and one that suspends wakes the request's own task. There is no thread
//! pool and no blocking bridge, and the handler future is [`Send`] on any runtime flavour
//! (including `#[tokio::test]`'s default `current_thread`). Children the fiber forks are spawned
//! on the runtime as usual.
//!
//! 0.6 ran each `!Send` v1 effect on a dedicated OS thread with its own `current_thread` runtime
//! (an elastic pool of up to 512 threads); that pool and its `max_threads`, `queue_capacity` and
//! `idle_timeout` knobs are gone.
//!
//! - **Interruption.** When the caller's future is dropped before the fiber is done (the client
//!   disconnected, a timeout layer gave up, the server shut down), the fiber is handed to the
//!   executor and **interrupted** ([`DropPolicy::Interrupt`], default): its finalizers
//!   (`ensuring`, `acquire_release`, scoped layers) run there. [`DropPolicy::Detach`] lets it
//!   run to completion instead (the result is discarded), the 0.6 default.
//! - **Panics** inside the build closure or the effect become a defect
//!   ([`Cause::Die`](id_effect::Cause::Die)), never an unwinding handler: the routing helpers
//!   answer `500` (see [`crate::exit_response`]).
//! - **Backpressure** is opt-in: with [`EffectExecutorBuilder::max_in_flight`] at most `n` request
//!   fibers run at once. Further callers wait ([`SaturationPolicy::Wait`], default) or the `try_*`
//!   entry points return [`Saturated`] ([`SaturationPolicy::Reject`]), which the routing helpers
//!   answer with `503`. The slot is held until the fiber has finished (finalizers included), also
//!   when it was detached.
//! - **Runtime.** By default each request runs on the tokio runtime that polls the handler (the
//!   one `axum::serve` or the test uses); [`EffectExecutorBuilder::runtime`] pins a [`Runtime`]
//!   instead, and each fiber is then spawned onto it. Outside any tokio runtime the process-wide
//!   [`Runtime::global`] is used the same way.
//!
//! Configure the process-wide executor with [`EffectExecutor::install_global`] before serving.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll};

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use id_effect::Exit;
use id_effect::{Effect, FiberContext, HNil, JoinFuture, Runtime, TokioExecutor, suspend, sync};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

static GLOBAL: OnceLock<EffectExecutor> = OnceLock::new();

/// What happens to a request's fiber when the caller's future is dropped before it is done.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DropPolicy {
  /// Interrupt the fiber; its finalizers run (default).
  #[default]
  Interrupt,
  /// Let the fiber run to completion; its result is discarded.
  Detach,
}

/// What the `try_*` entry points do when [`EffectExecutorBuilder::max_in_flight`] fibers run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SaturationPolicy {
  /// Wait for a free slot (default). The non-`try` entry points always wait.
  #[default]
  Wait,
  /// Fail fast with [`Saturated`] (mapped to `503` by the routing helpers).
  Reject,
}

/// The executor is at its in-flight limit and its [`SaturationPolicy`] is
/// [`SaturationPolicy::Reject`].
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
#[derive(Clone, Debug, Default)]
pub struct EffectExecutorBuilder {
  max_in_flight: Option<usize>,
  drop_policy: DropPolicy,
  saturation: SaturationPolicy,
  runtime: Option<Runtime>,
}

impl EffectExecutorBuilder {
  /// At most `n` (at least 1) request fibers run at once; unbounded by default.
  #[must_use]
  pub fn max_in_flight(mut self, n: usize) -> Self {
    self.max_in_flight = Some(n.max(1));
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

  /// Run every request's fiber on `runtime` instead of the tokio runtime polling the handler.
  #[must_use]
  pub fn runtime(mut self, runtime: impl Into<Runtime>) -> Self {
    self.runtime = Some(runtime.into());
    self
  }

  /// Build the executor.
  pub fn build(self) -> EffectExecutor {
    let admission = self
      .max_in_flight
      .map(|n| Arc::new(Semaphore::new(n.min(Semaphore::MAX_PERMITS))));
    EffectExecutor {
      inner: Arc::new(Inner {
        admission,
        in_flight: Arc::new(AtomicUsize::new(0)),
        config: self,
      }),
    }
  }
}

struct Inner {
  config: EffectExecutorBuilder,
  admission: Option<Arc<Semaphore>>,
  in_flight: Arc<AtomicUsize>,
}

/// Runs request effects as fibers on the [`TokioExecutor`] (see the [module docs](self)).
///
/// Cheap to clone (clones share the in-flight limit). Most code uses [`EffectExecutor::global`]
/// implicitly through [`crate::routing`], [`crate::execute`], [`crate::run_with_env`] and the
/// `service` module.
#[derive(Clone)]
pub struct EffectExecutor {
  inner: Arc<Inner>,
}

impl Default for EffectExecutor {
  fn default() -> Self {
    Self::new()
  }
}

impl fmt::Debug for EffectExecutor {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("EffectExecutor")
      .field("in_flight", &self.in_flight())
      .field("config", &self.inner.config)
      .finish()
  }
}

impl EffectExecutor {
  /// Executor with the default configuration (unbounded, [`DropPolicy::Interrupt`], ambient
  /// tokio runtime).
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

  /// Request fibers started by this executor (and its clones) that have not finished yet.
  pub fn in_flight(&self) -> usize {
    self.inner.in_flight.load(Ordering::SeqCst)
  }

  /// The configured in-flight limit (`None`: unbounded).
  pub fn max_in_flight(&self) -> Option<usize> {
    self.inner.config.max_in_flight
  }

  /// The configured [`DropPolicy`].
  pub fn drop_policy(&self) -> DropPolicy {
    self.inner.config.drop_policy
  }

  /// The configured [`SaturationPolicy`].
  pub fn saturation(&self) -> SaturationPolicy {
    self.inner.config.saturation
  }

  /// The runtime a fiber started now runs on: the configured one, else a [`TokioExecutor`] over
  /// the current tokio runtime, else [`Runtime::global`].
  pub fn runtime(&self) -> Runtime {
    if let Some(rt) = &self.inner.config.runtime {
      return rt.clone();
    }
    match TokioExecutor::current() {
      Some(exec) => Runtime::from_executor(Arc::new(exec)),
      None => Runtime::global().clone(),
    }
  }

  /// Runs `build(&mut env)` and the effect it returns, provided with the same `env`, as one
  /// request fiber. The build closure runs inside the fiber, so a panic there is a defect too.
  ///
  /// Always waits for a slot when an in-flight limit is set (use [`Self::try_run`] to honour
  /// [`SaturationPolicy::Reject`]).
  pub fn run<S, A, E, F>(
    &self,
    env: S,
    build: F,
  ) -> impl Future<Output = Exit<A, E>> + Send + 'static
  where
    S: Send + Sync + 'static,
    A: Send + 'static,
    E: Send + 'static,
    F: FnOnce(&mut S) -> Effect<A, E, S> + Send + 'static,
  {
    self.run_effect(request_effect(env, build))
  }

  /// [`Self::run`], returning [`Saturated`] instead of waiting when the executor is at its limit
  /// and configured with [`SaturationPolicy::Reject`].
  pub fn try_run<S, A, E, F>(
    &self,
    env: S,
    build: F,
  ) -> impl Future<Output = Result<Exit<A, E>, Saturated>> + Send + 'static
  where
    S: Send + Sync + 'static,
    A: Send + 'static,
    E: Send + 'static,
    F: FnOnce(&mut S) -> Effect<A, E, S> + Send + 'static,
  {
    self.try_run_effect(request_effect(env, build))
  }

  /// Runs a ready-to-run effect (`R = HNil`) as one request fiber and awaits its exit. Waits for
  /// a slot when an in-flight limit is set.
  pub fn run_effect<A, E>(
    &self,
    eff: Effect<A, E, HNil>,
  ) -> impl Future<Output = Exit<A, E>> + Send + 'static
  where
    A: Send + 'static,
    E: Send + 'static,
  {
    let this = self.clone();
    async move {
      let permit = match &this.inner.admission {
        None => None,
        Some(sem) => Some(acquire(sem).await),
      };
      this.start(eff, permit).await
    }
  }

  /// [`Self::run_effect`] honouring [`SaturationPolicy::Reject`].
  pub fn try_run_effect<A, E>(
    &self,
    eff: Effect<A, E, HNil>,
  ) -> impl Future<Output = Result<Exit<A, E>, Saturated>> + Send + 'static
  where
    A: Send + 'static,
    E: Send + 'static,
  {
    let this = self.clone();
    async move {
      let permit = match (&this.inner.admission, this.saturation()) {
        (None, _) => None,
        (Some(sem), SaturationPolicy::Wait) => Some(acquire(sem).await),
        (Some(sem), SaturationPolicy::Reject) => {
          Some(Arc::clone(sem).try_acquire_owned().map_err(|_| Saturated)?)
        }
      };
      Ok(this.start(eff, permit).await)
    }
  }

  /// Starts `eff` (plus the bookkeeping finalizer) as a root fiber; the returned future awaits it
  /// and interrupts it when dropped early (under [`DropPolicy::Interrupt`]).
  ///
  /// On the ambient tokio runtime (no [`EffectExecutorBuilder::runtime`] pinned) the fiber is
  /// driven inline by the returned future, on the caller's task, so an effect that completes
  /// without suspending costs no task spawn and no cross-thread wake-up. It is handed to the
  /// executor only when the caller drops the future before it is done: interrupted first under
  /// [`DropPolicy::Interrupt`] (its finalizers run there), left to finish under
  /// [`DropPolicy::Detach`]. A pinned runtime, or none at all, forks the fiber onto it instead.
  fn start<A, E>(
    &self,
    eff: Effect<A, E, HNil>,
    permit: Option<OwnedSemaphorePermit>,
  ) -> RequestFiber<A, E>
  where
    A: Send + 'static,
    E: Send + 'static,
  {
    let in_flight = Arc::clone(&self.inner.in_flight);
    in_flight.fetch_add(1, Ordering::SeqCst);
    let done = sync(move || {
      drop(permit);
      in_flight.fetch_sub(1, Ordering::SeqCst);
    });
    let eff = eff.ensuring(done);
    let ambient = match self.inner.config.runtime {
      Some(_) => None,
      None => TokioExecutor::current(),
    };
    let (fiber, drive) = match ambient {
      Some(exec) => {
        let fiber = FiberContext::new_root(Arc::new(exec));
        let run: InlineRun<A, E> = Box::pin(eff.run_fiber(Arc::clone(&fiber)));
        (fiber, Drive::Inline(run))
      }
      None => {
        let handle = self.runtime().fork(eff);
        (
          Arc::clone(handle.fiber()),
          Drive::Forked(handle.join_future()),
        )
      }
    };
    RequestFiber {
      fiber,
      policy: self.drop_policy(),
      drive: Some(drive),
    }
  }
}

async fn acquire(sem: &Arc<Semaphore>) -> OwnedSemaphorePermit {
  Arc::clone(sem)
    .acquire_owned()
    .await
    .expect("effect executor admission semaphore is never closed")
}

/// `build(&mut env)` provided with `env`, built lazily inside the fiber (a panicking build is a
/// defect, not an unwinding caller).
pub(crate) fn request_effect<S, A, E, F>(env: S, build: F) -> Effect<A, E, HNil>
where
  S: Send + Sync + 'static,
  A: Send + 'static,
  E: Send + 'static,
  F: FnOnce(&mut S) -> Effect<A, E, S> + Send + 'static,
{
  suspend(move || {
    let mut env = env;
    let eff = build(&mut env);
    eff.provide_all(env)
  })
}

/// A root fiber driven by the request future itself (see [`EffectExecutor::start`]).
type InlineRun<A, E> = Pin<Box<dyn Future<Output = Exit<A, E>> + Send>>;

/// How a request fiber makes progress.
enum Drive<A, E> {
  /// Polled by the caller; spawned onto the fiber's executor if the caller gives up early.
  Inline(InlineRun<A, E>),
  /// Running as its own task; the caller awaits its exit.
  Forked(JoinFuture<A, E>),
}

/// The exit of one request fiber; interrupts the fiber when dropped before it is done (unless
/// detached).
struct RequestFiber<A: Send + 'static, E: Send + 'static> {
  fiber: Arc<FiberContext>,
  policy: DropPolicy,
  /// `None` once the exit was returned.
  drive: Option<Drive<A, E>>,
}

impl<A: Send + 'static, E: Send + 'static> Future for RequestFiber<A, E> {
  type Output = Exit<A, E>;

  fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Exit<A, E>> {
    let exit = match self.drive.as_mut() {
      Some(Drive::Inline(run)) => std::task::ready!(run.as_mut().poll(cx)),
      Some(Drive::Forked(join)) => std::task::ready!(Pin::new(join).poll(cx)),
      None => unreachable!("request fiber polled after completion"),
    };
    self.drive = None;
    Poll::Ready(exit)
  }
}

impl<A: Send + 'static, E: Send + 'static> Drop for RequestFiber<A, E> {
  fn drop(&mut self) {
    let interrupt = self.policy == DropPolicy::Interrupt;
    match self.drive.take() {
      None => {}
      Some(Drive::Forked(_)) => {
        if interrupt && !self.fiber.is_done() {
          self.fiber.request_interrupt();
        }
      }
      // Not done yet (it would have returned its exit): the executor finishes it, finalizers
      // included, the way a forked fiber would.
      Some(Drive::Inline(run)) => {
        if interrupt {
          self.fiber.request_interrupt();
        }
        self.fiber.executor().spawn(Box::pin(async move {
          let _ = run.await;
        }));
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect::Cause;
  use id_effect::{fail, from_async, sleep, succeed};
  use std::sync::atomic::AtomicBool;
  use std::time::Duration;
  use tokio::sync::oneshot;

  fn assert_send<T: Send>(_: &T) {}

  const PATIENCE: Duration = Duration::from_secs(10);

  #[tokio::test(flavor = "current_thread")]
  async fn run_works_on_current_thread_runtime() {
    let exec = EffectExecutor::new();
    let out = exec
      .run(5_u32, |n: &mut u32| succeed::<_, (), _>(*n * 2))
      .await;
    assert_eq!(out, Exit::Success(10));
    let err = exec.run((), |_: &mut ()| fail::<u8, _, _>("nope")).await;
    assert_eq!(err, Exit::Failure(Cause::Fail("nope")));
  }

  #[tokio::test(flavor = "current_thread")]
  async fn run_future_is_send_and_awaits_async_steps() {
    let fut = EffectExecutor::global().run((), |_: &mut ()| {
      sleep::<(), ()>(Duration::from_millis(1)).map(|()| 3_u8)
    });
    assert_send(&fut);
    assert_eq!(fut.await, Exit::Success(3));
  }

  #[tokio::test(flavor = "current_thread")]
  async fn caller_thread_is_never_blocked() {
    // The effect waits on a flag the caller's (single) thread sets afterwards.
    let exec = EffectExecutor::new();
    let flag = Arc::new(AtomicBool::new(false));
    let f2 = flag.clone();
    let run = exec.run((), move |_: &mut ()| {
      from_async(move || async move {
        while !f2.load(Ordering::SeqCst) {
          tokio::time::sleep(Duration::from_millis(1)).await;
        }
        Ok::<(), ()>(())
      })
    });
    let setter = async {
      tokio::time::sleep(Duration::from_millis(5)).await;
      flag.store(true, Ordering::SeqCst);
    };
    let (out, ()) = tokio::join!(run, setter);
    assert_eq!(out, Exit::Success(()));
  }

  /// Fires its sender when dropped.
  struct DropSignal(Option<oneshot::Sender<()>>);

  impl Drop for DropSignal {
    fn drop(&mut self) {
      if let Some(tx) = self.0.take() {
        let _ = tx.send(());
      }
    }
  }

  async fn wait_idle(exec: &EffectExecutor) {
    tokio::time::timeout(PATIENCE, async {
      while exec.in_flight() > 0 {
        tokio::time::sleep(Duration::from_millis(1)).await;
      }
    })
    .await
    .expect("in-flight count returns to zero");
  }

  #[tokio::test(flavor = "current_thread")]
  async fn dropping_the_caller_interrupts_the_fiber_and_runs_finalizers() {
    let exec = EffectExecutor::new();
    assert_eq!(exec.drop_policy(), DropPolicy::Interrupt);
    let (started_tx, started_rx) = oneshot::channel::<()>();
    let (fin_tx, fin_rx) = oneshot::channel::<()>();
    let finished = Arc::new(AtomicBool::new(false));
    let f2 = finished.clone();
    let caller = tokio::spawn(exec.run((), move |_: &mut ()| {
      let signal = DropSignal(Some(fin_tx));
      sync::<_, (), ()>(move || {
        let _ = started_tx.send(());
      })
      .then(sleep(Duration::from_secs(3600)))
      .map(move |()| f2.store(true, Ordering::SeqCst))
      .ensuring(sync(move || drop(signal)))
    }));
    started_rx.await.unwrap();
    caller.abort(); // client disconnect
    assert!(caller.await.unwrap_err().is_cancelled());
    tokio::time::timeout(PATIENCE, fin_rx)
      .await
      .expect("the finalizer runs once the caller is gone")
      .unwrap();
    assert!(!finished.load(Ordering::SeqCst));
    wait_idle(&exec).await;
  }

  #[tokio::test(flavor = "current_thread")]
  async fn detach_policy_runs_the_effect_to_completion() {
    let exec = EffectExecutor::builder()
      .drop_policy(DropPolicy::Detach)
      .build();
    let (started_tx, started_rx) = oneshot::channel::<()>();
    let (release_tx, release_rx) = oneshot::channel::<()>();
    let (finished_tx, finished_rx) = oneshot::channel::<()>();
    let caller = tokio::spawn(exec.run((), move |_: &mut ()| {
      from_async(move || async move {
        let _ = started_tx.send(());
        let _ = release_rx.await;
        let _ = finished_tx.send(());
        Ok::<(), ()>(())
      })
    }));
    started_rx.await.unwrap();
    caller.abort();
    let _ = caller.await;
    release_tx.send(()).unwrap();
    tokio::time::timeout(PATIENCE, finished_rx)
      .await
      .expect("a detached effect finishes although its caller is gone")
      .unwrap();
    wait_idle(&exec).await;
  }

  #[tokio::test(flavor = "current_thread")]
  async fn an_inline_fiber_dropped_after_starting_finishes_on_the_executor() {
    // Polled once by hand (the fiber starts inline and suspends), then dropped: the executor
    // takes it over, interrupts it and runs its finalizer.
    let exec = EffectExecutor::new();
    let (fin_tx, fin_rx) = oneshot::channel::<()>();
    let ran = Arc::new(AtomicBool::new(false));
    let r2 = ran.clone();
    let mut run = Box::pin(exec.run((), move |_: &mut ()| {
      let signal = DropSignal(Some(fin_tx));
      sync::<_, (), ()>(move || r2.store(true, Ordering::SeqCst))
        .then(sleep(Duration::from_secs(3600)))
        .ensuring(sync(move || drop(signal)))
    }));
    assert!(futures_poll_once(run.as_mut()).await.is_pending());
    assert!(
      ran.load(Ordering::SeqCst),
      "the effect ran inline on the first poll"
    );
    assert_eq!(exec.in_flight(), 1);
    drop(run);
    tokio::time::timeout(PATIENCE, fin_rx)
      .await
      .expect("the finalizer runs on the executor")
      .unwrap();
    wait_idle(&exec).await;
  }

  /// Polls `fut` once with the current task's context.
  async fn futures_poll_once<F: Future + ?Sized>(fut: Pin<&mut F>) -> Poll<F::Output> {
    let mut fut = Some(fut);
    std::future::poll_fn(move |cx| Poll::Ready(fut.take().expect("polled once").poll(cx))).await
  }

  #[tokio::test(flavor = "current_thread")]
  async fn a_pinned_runtime_runs_the_fiber_as_its_own_task() {
    let exec = EffectExecutor::builder()
      .runtime(Runtime::global().clone())
      .build();
    let out = exec
      .run((), |_: &mut ()| {
        sleep::<(), ()>(Duration::from_millis(1)).map(|()| 6_u8)
      })
      .await;
    assert_eq!(out, Exit::Success(6));
    wait_idle(&exec).await;
  }

  #[tokio::test(flavor = "current_thread")]
  async fn reject_policy_returns_saturated_then_recovers() {
    let exec = EffectExecutor::builder()
      .max_in_flight(1)
      .saturation(SaturationPolicy::Reject)
      .build();
    let (started_tx, started_rx) = oneshot::channel::<()>();
    let (release_tx, release_rx) = oneshot::channel::<()>();
    let busy = tokio::spawn(exec.try_run((), move |_: &mut ()| {
      from_async(move || async move {
        let _ = started_tx.send(());
        let _ = release_rx.await;
        Ok::<u8, ()>(1)
      })
    }));
    started_rx.await.unwrap();
    let rejected = exec
      .try_run((), |_: &mut ()| succeed::<_, (), _>(2_u8))
      .await;
    assert_eq!(rejected, Err(Saturated));
    release_tx.send(()).unwrap();
    assert_eq!(busy.await.unwrap(), Ok(Exit::Success(1)));
    let ok = exec
      .try_run((), |_: &mut ()| succeed::<_, (), _>(3_u8))
      .await;
    assert_eq!(ok, Ok(Exit::Success(3)));
  }

  #[tokio::test(flavor = "current_thread")]
  async fn wait_policy_queues_until_a_slot_frees_up() {
    let exec = EffectExecutor::builder().max_in_flight(1).build();
    assert_eq!(exec.max_in_flight(), Some(1));
    let (started_tx, started_rx) = oneshot::channel::<()>();
    let (release_tx, release_rx) = oneshot::channel::<()>();
    let busy = tokio::spawn(exec.run((), move |_: &mut ()| {
      from_async(move || async move {
        let _ = started_tx.send(());
        let _ = release_rx.await;
        Ok::<u8, ()>(1)
      })
    }));
    started_rx.await.unwrap();
    let mut waiting = tokio::spawn(exec.try_run((), |_: &mut ()| succeed::<_, (), _>(2_u8)));
    assert!(
      tokio::time::timeout(Duration::from_millis(20), &mut waiting)
        .await
        .is_err(),
      "second request waits while the only slot is busy"
    );
    release_tx.send(()).unwrap();
    assert_eq!(busy.await.unwrap(), Exit::Success(1));
    assert_eq!(waiting.await.unwrap(), Ok(Exit::Success(2)));
  }

  fn die_message<A, E>(exit: &Exit<A, E>) -> Option<String> {
    match exit {
      Exit::Failure(Cause::Die(d)) => Some(d.message().to_string()),
      _ => None,
    }
  }

  #[tokio::test(flavor = "current_thread")]
  async fn a_panicking_build_or_effect_is_a_defect() {
    let exec = EffectExecutor::new();
    let in_build = exec
      .run((), |_: &mut ()| -> Effect<(), (), ()> {
        panic!("boom in build")
      })
      .await;
    assert_eq!(die_message(&in_build).as_deref(), Some("boom in build"));
    let in_effect = exec
      .run((), |_: &mut ()| {
        sync::<(), (), ()>(|| panic!("boom in effect"))
      })
      .await;
    assert_eq!(die_message(&in_effect).as_deref(), Some("boom in effect"));
    // The executor keeps serving.
    assert_eq!(
      exec.run((), |_: &mut ()| succeed::<_, (), _>(7)).await,
      Exit::Success(7)
    );
  }

  #[test]
  fn builder_clamps_debug_runtime_and_saturated_response() {
    let exec = EffectExecutor::builder().max_in_flight(0).build();
    let rendered = format!("{exec:?}");
    assert!(rendered.contains("max_in_flight: Some(1)"), "{rendered}");
    assert_eq!(exec.saturation(), SaturationPolicy::Wait);
    // Outside tokio: the global runtime.
    let _ = exec.runtime();
    let pinned = EffectExecutor::builder()
      .runtime(Runtime::global().clone())
      .build();
    let _ = pinned.runtime();
    let res = Saturated.into_response();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(res.headers()[header::RETRY_AFTER], "1");
    assert_eq!(Saturated.to_string(), "effect executor saturated");
    let _ = EffectExecutor::global();
    assert!(EffectExecutor::install_global(EffectExecutor::default()).is_err());
  }
}
