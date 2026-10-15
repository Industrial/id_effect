//! [`EffectExecutor`]: runs one [`id_effect::Effect`] per request as a fiber on the
//! [`TokioExecutor`] of the runtime that serves the request.
//!
//! ## How
//!
//! Effects are [`Send`], so they run where Axum already runs: every request's effect is
//! forked as a root fiber on the ambient tokio runtime (a [`TokioExecutor`] over
//! [`tokio::runtime::Handle::current`]), and the handler future awaits the fiber's [`Exit`]. There
//! is no thread pool and no blocking bridge: a suspended effect costs a parked tokio task, and the
//! handler future is [`Send`] on any runtime flavour (including `#[tokio::test]`'s default
//! `current_thread`).
//!
//! 0.6 ran each `!Send` v1 effect on a dedicated OS thread with its own `current_thread` runtime
//! (an elastic pool of up to 512 threads); that pool and its `max_threads`, `queue_capacity` and
//! `idle_timeout` knobs are gone.
//!
//! - **Interruption.** When the caller's future is dropped before the fiber is done (the client
//!   disconnected, a timeout layer gave up, the server shut down), the fiber is **interrupted**
//!   ([`DropPolicy::Interrupt`], default): its finalizers (`ensuring`, `acquire_release`, scoped
//!   layers) run on the executor. [`DropPolicy::Detach`] lets it run to completion instead (the
//!   result is discarded), the 0.6 default.
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
//!   instead. Outside any tokio runtime the process-wide [`Runtime::global`] is used.
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

  /// Forks `eff` (plus the bookkeeping finalizer) as a root fiber; the returned future awaits it
  /// and interrupts it when dropped early (under [`DropPolicy::Interrupt`]).
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
    let handle = self.runtime().fork(eff.ensuring(done));
    let fiber = match self.drop_policy() {
      DropPolicy::Interrupt => Some(Arc::clone(handle.fiber())),
      DropPolicy::Detach => None,
    };
    RequestFiber {
      fiber,
      join: handle.join_future(),
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

/// The exit of one request fiber; interrupts the fiber when dropped before it is done (unless
/// detached).
struct RequestFiber<A, E> {
  fiber: Option<Arc<FiberContext>>,
  join: JoinFuture<A, E>,
}

impl<A: Send + 'static, E: Send + 'static> Future for RequestFiber<A, E> {
  type Output = Exit<A, E>;

  fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Exit<A, E>> {
    let exit = std::task::ready!(Pin::new(&mut self.join).poll(cx));
    self.fiber = None;
    Poll::Ready(exit)
  }
}

impl<A, E> Drop for RequestFiber<A, E> {
  fn drop(&mut self) {
    if let Some(fiber) = self.fiber.take()
      && !fiber.is_done()
    {
      fiber.request_interrupt();
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
