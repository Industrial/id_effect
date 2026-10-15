//! [`tower::Service`] implementations that run [`id_effect::Effect`] programs, so handlers
//! stay in the effect runtime while composing with Tower middleware stacks.
//!
//! Enabled by the `tower` feature. Each call runs as one request fiber on the
//! [`EffectExecutor`] (the global one unless [`EffectService::with_executor`] is used), so service
//! futures are [`Send`], work on any Tokio runtime flavour, and dropping one interrupts its fiber
//! (under [`DropPolicy::Interrupt`](crate::DropPolicy::Interrupt)).
//!
//! [`EffectService`]'s error type is the whole [`Cause<E>`]: a typed failure is `Cause::Fail(e)`,
//! a panic in the handler is `Cause::Die(..)`, an interrupt `Cause::Interrupt(..)`.
//!
//! ## Concurrency
//!
//! [`EffectService::with_max_in_flight`] adds a [`tokio::sync::Semaphore`] gate so
//! [`Service::poll_ready`] acquires a permit before each [`Service::call`]. In-flight work is
//! tracked in a [`SynchronizedRef`]; the count and the permit are released by a finalizer of the
//! request fiber, so every exit path (success, failure, defect, interruption) releases them.
//!
//! [`EffectService::with_request_metrics`] records each call's duration (seconds) in a latency
//! [`Histogram`] and counts typed failures in an error [`Counter`].
//!
//! ## Examples
//!
//! See `cargo run -p id_effect_axum --features tower --example 050_tower_effect_service`.

use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use id_effect::{Cause, Exit};
use id_effect::{Channel, Counter, Effect, Histogram, Metric, QueueError, SynchronizedRef, sync};
use tokio::sync::{AcquireError, OwnedSemaphorePermit, Semaphore};
use tower::Service;

use crate::EffectExecutor;

type AcquireFuture =
  Pin<Box<dyn Future<Output = Result<OwnedSemaphorePermit, AcquireError>> + Send>>;

/// Optional per-request latency histogram and error-rate counter.
#[derive(Clone)]
struct RequestMetrics {
  latency: Histogram,
  errors: Counter,
}

/// Shared limiter for [`EffectService::with_max_in_flight`].
struct ServiceLimit {
  sem: Arc<Semaphore>,
  in_flight: SynchronizedRef<u64>,
}

impl ServiceLimit {
  fn new(max_in_flight: usize) -> Self {
    assert!(max_in_flight > 0, "max_in_flight must be positive");
    Self {
      sem: Arc::new(Semaphore::new(max_in_flight)),
      in_flight: SynchronizedRef::new(0_u64),
    }
  }
}

/// [`Service`] that clones `S`, builds `f(&mut env, req)` and runs it, provided with `env`, as a
/// request fiber.
///
/// `S` is typically application state (`Clone`, often `Arc`-backed) or a capability list,
/// matching Axum’s pattern.
pub struct EffectService<S, F, Req> {
  state: S,
  f: F,
  limit: Option<Arc<ServiceLimit>>,
  request_metrics: Option<RequestMetrics>,
  /// Permit reserved by [`Service::poll_ready`](tower::Service::poll_ready) for the next
  /// [`Service::call`](tower::Service::call) when a concurrency limit is set.
  ready_permit: Option<OwnedSemaphorePermit>,
  pending_acquire: Option<AcquireFuture>,
  executor: Option<EffectExecutor>,
  _pd: PhantomData<fn(Req) -> ()>,
}

impl<S, F, Req> EffectService<S, F, Req> {
  /// Build an unlimited-concurrency service with handler closure `f` and shared state `state`.
  #[inline]
  pub fn new(state: S, f: F) -> Self {
    Self {
      state,
      f,
      limit: None,
      request_metrics: None,
      ready_permit: None,
      pending_acquire: None,
      executor: None,
      _pd: PhantomData,
    }
  }

  /// Record each request's duration in `latency` and increment `errors` when the handler effect
  /// fails with a typed error.
  #[must_use]
  #[inline]
  pub fn with_request_metrics(mut self, latency: Histogram, errors: Counter) -> Self {
    self.request_metrics = Some(RequestMetrics { latency, errors });
    self
  }

  /// Same as [`Self::new`], but at most `max_in_flight` requests run concurrently.
  ///
  /// [`Service::poll_ready`] acquires a semaphore permit (possibly after pending) before
  /// reporting readiness; [`Service::call`] hands that permit to the request fiber, which
  /// releases it when it ends.
  ///
  /// # Panics
  ///
  /// Panics if `max_in_flight == 0`.
  #[inline]
  pub fn with_max_in_flight(state: S, max_in_flight: usize, f: F) -> Self {
    Self {
      limit: Some(Arc::new(ServiceLimit::new(max_in_flight))),
      ..Self::new(state, f)
    }
  }

  /// Run handler effects on `executor` instead of [`EffectExecutor::global`] (e.g. one configured
  /// with [`crate::DropPolicy::Detach`]). Service calls always wait for executor admission.
  #[must_use]
  #[inline]
  pub fn with_executor(mut self, executor: EffectExecutor) -> Self {
    self.executor = Some(executor);
    self
  }

  /// Current in-flight count when this service was created with [`Self::with_max_in_flight`].
  ///
  /// Returns `None` if there is no concurrency limit (unlimited [`Self::new`]).
  pub fn in_flight_counter(&self) -> Option<SynchronizedRef<u64>> {
    self.limit.as_ref().map(|l| l.in_flight.clone())
  }

  /// Immutable reference to the service’s shared state `S`.
  #[inline]
  pub fn state(&self) -> &S {
    &self.state
  }

  /// Mutable reference to the service’s shared state `S`.
  #[inline]
  pub fn state_mut(&mut self) -> &mut S {
    &mut self.state
  }

  fn poll_ready_limited(
    &mut self,
    cx: &mut Context<'_>,
    lim: &Arc<ServiceLimit>,
  ) -> Poll<Result<(), AcquireError>> {
    if self.ready_permit.is_some() {
      return Poll::Ready(Ok(()));
    }
    match lim.sem.clone().try_acquire_owned() {
      Ok(permit) => {
        self.pending_acquire = None;
        self.ready_permit = Some(permit);
        Poll::Ready(Ok(()))
      }
      Err(_) => {
        let fut = self
          .pending_acquire
          .get_or_insert_with(|| Box::pin(lim.sem.clone().acquire_owned()));
        match fut.as_mut().poll(cx) {
          Poll::Ready(Ok(permit)) => {
            self.pending_acquire = None;
            self.ready_permit = Some(permit);
            Poll::Ready(Ok(()))
          }
          Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
          Poll::Pending => Poll::Pending,
        }
      }
    }
  }
}

impl<S, F, Req> Clone for EffectService<S, F, Req>
where
  S: Clone,
  F: Clone,
{
  fn clone(&self) -> Self {
    Self {
      state: self.state.clone(),
      f: self.f.clone(),
      limit: self.limit.clone(),
      request_metrics: self.request_metrics.clone(),
      ready_permit: None,
      pending_acquire: None,
      executor: self.executor.clone(),
      _pd: PhantomData,
    }
  }
}

impl<S, F, Req, Res, E> Service<Req> for EffectService<S, F, Req>
where
  S: Clone + Send + Sync + 'static,
  Req: Send + 'static,
  Res: Send + 'static,
  E: Send + 'static,
  F: Fn(&mut S, Req) -> Effect<Res, E, S> + Clone + Send + 'static,
{
  type Response = Res;
  type Error = Cause<E>;
  /// [`Send`]: the effect runs as a request fiber; dropping the future interrupts it.
  type Future = Pin<Box<dyn Future<Output = Result<Res, Cause<E>>> + Send + 'static>>;

  fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
    match self.limit.clone() {
      None => Poll::Ready(Ok(())),
      // The semaphore is never closed; a closed one would surface as a missing permit in `call`.
      Some(lim) => self.poll_ready_limited(cx, &lim).map(|_| Ok(())),
    }
  }

  fn call(&mut self, req: Req) -> Self::Future {
    let env = self.state.clone();
    let f = self.f.clone();
    let metrics = self.request_metrics.clone();
    let executor = self
      .executor
      .clone()
      .unwrap_or_else(|| EffectExecutor::global().clone());
    let limited = self.limit.as_ref().map(|lim| {
      let permit = self
        .ready_permit
        .take()
        .expect("EffectService::call without successful poll_ready");
      (permit, lim.in_flight.clone())
    });
    let run = executor.run(env, move |e: &mut S| {
      let mut eff = f(e, req);
      if let Some(m) = metrics {
        eff = m
          .latency
          .track_duration(m.errors.track_error_with(eff, |_| 1));
      }
      match limited {
        None => eff,
        Some((permit, in_flight)) => {
          let release = in_flight
            .update(|n| n.saturating_sub(1))
            .flat_map(move |()| sync(move || drop(permit)));
          in_flight
            .update(|n| n.saturating_add(1))
            .then(eff)
            .ensuring(release)
        }
      }
    });
    Box::pin(async move {
      match run.await {
        Exit::Success(res) => Ok(res),
        Exit::Failure(cause) => Err(cause),
      }
    })
  }
}

/// Tower [`Service`] over a queue-backed [`Channel`]: each [`Service::call`] writes the request
/// and reads the next response (as one request fiber). A read on a shut-down, drained channel
/// (or an interrupted call) is [`QueueError::Disconnected`].
pub struct ChannelService<Req, Res> {
  channel: Channel<Res, Req>,
}

impl<Req, Res> ChannelService<Req, Res>
where
  Req: Send + 'static,
  Res: Send + 'static,
{
  /// Service over the shared `channel`.
  #[inline]
  pub fn new(channel: Channel<Res, Req>) -> Self {
    Self { channel }
  }

  /// Borrow the underlying [`Channel`].
  #[inline]
  pub fn channel(&self) -> &Channel<Res, Req> {
    &self.channel
  }
}

impl<Req, Res> Clone for ChannelService<Req, Res> {
  fn clone(&self) -> Self {
    Self {
      channel: self.channel.clone(),
    }
  }
}

impl<Req, Res> Service<Req> for ChannelService<Req, Res>
where
  Req: Send + 'static,
  Res: Send + 'static,
{
  type Response = Res;
  type Error = QueueError;
  type Future = Pin<Box<dyn Future<Output = Result<Res, QueueError>> + Send + 'static>>;

  fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
    Poll::Ready(Ok(()))
  }

  fn call(&mut self, req: Req) -> Self::Future {
    let ch = self.channel.clone();
    let run = EffectExecutor::global()
      .run_effect(ch.write::<std::convert::Infallible, _>(req).then(ch.read()));
    Box::pin(async move {
      match run.await {
        Exit::Success(Some(res)) => Ok(res),
        Exit::Success(None) | Exit::Failure(_) => Err(QueueError::Disconnected),
      }
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect::{Boundaries, HNil, fail, from_async, sleep, succeed};
  use std::convert::Infallible;
  use std::time::Duration;
  use tower::{Service, ServiceExt};

  async fn read(ctr: &SynchronizedRef<u64>) -> u64 {
    match EffectExecutor::global()
      .run_effect(ctr.get::<Infallible, HNil>())
      .await
    {
      Exit::Success(n) => n,
      Exit::Failure(c) => panic!("read failed: {c:?}"),
    }
  }

  async fn wait_for_zero(ctr: &SynchronizedRef<u64>) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
      let n = read(ctr).await;
      if n == 0 {
        return;
      }
      assert!(
        std::time::Instant::now() < deadline,
        "in_flight stuck at {n}"
      );
      tokio::time::sleep(Duration::from_millis(5)).await;
    }
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn tower_channel_service_returns_response() {
    let mut svc = ChannelService::new(Channel::<u32>::duplex_unbounded());
    assert_eq!(svc.ready().await.unwrap().call(41).await, Ok(41));
    let _ = svc.channel();
  }

  #[tokio::test(flavor = "current_thread")]
  async fn channel_service_reports_disconnect_after_shutdown() {
    let ch = Channel::<u32>::duplex_unbounded();
    let _ = EffectExecutor::global()
      .run_effect(ch.shutdown::<Infallible, HNil>())
      .await;
    let mut svc = ChannelService::new(ch);
    assert_eq!(
      svc.ready().await.unwrap().call(1).await,
      Err(QueueError::Disconnected)
    );
  }

  #[tokio::test(flavor = "current_thread")]
  async fn service_runs_effect() {
    let mut svc = EffectService::new((), |_env: &mut (), x: u32| {
      succeed::<u32, (), _>(x.saturating_add(1))
    });
    assert_eq!(svc.ready().await.unwrap().call(10).await, Ok(11));
  }

  #[tokio::test(flavor = "current_thread")]
  async fn tower_latency_histogram_records_duration() {
    let latency = Histogram::new(
      "id_effect_axum_tower_req",
      Boundaries::exponential(0.0001, 2.0, 16),
    );
    let errors = Counter::new("id_effect_axum_tower_err");
    let mut svc = EffectService::new((), |_env: &mut (), _x: u32| {
      sleep::<(), ()>(Duration::from_millis(5)).map(|()| 7_u32)
    })
    .with_request_metrics(latency.clone(), errors.clone());

    assert_eq!(svc.ready().await.unwrap().call(0).await, Ok(7));
    let snap = latency.snapshot();
    assert_eq!(snap.count, 1);
    assert!(snap.sum > 0.0);
    assert_eq!(errors.count(), 0);
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  async fn tower_service_concurrent_calls_count_correctly() {
    let svc = EffectService::with_max_in_flight((), 4_usize, |_env: &mut (), _req: u32| {
      sleep::<(), ()>(Duration::from_millis(20)).map(|()| 1_u32)
    });
    let mut handles = Vec::new();
    for i in 0..8_u32 {
      let mut s = svc.clone();
      handles.push(tokio::spawn(async move {
        s.ready().await.unwrap();
        s.call(i).await
      }));
    }
    for h in handles {
      assert_eq!(h.await.unwrap(), Ok(1));
    }
    let ctr = svc.in_flight_counter().expect("counter");
    assert_eq!(read(&ctr).await, 0, "in_flight should return to zero");
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  async fn tower_service_ready_blocks_until_capacity() {
    let mut slow = EffectService::with_max_in_flight((), 1_usize, |_env: &mut (), _req: ()| {
      sleep::<(), ()>(Duration::from_millis(200))
    });
    let mut fast = slow.clone();

    slow.ready().await.unwrap();
    let long = slow.call(());

    let short_timeout = tokio::time::timeout(Duration::from_millis(40), fast.ready());
    assert!(
      short_timeout.await.is_err(),
      "second ready() should not complete while the single permit is held"
    );

    long.await.unwrap();

    tokio::time::timeout(Duration::from_secs(2), fast.ready())
      .await
      .expect("ready should complete after slow call finishes")
      .unwrap();
  }

  #[tokio::test(flavor = "current_thread")]
  async fn tower_error_metric_increments_on_failure() {
    let latency = Histogram::new(
      "id_effect_axum_tower_req_fail",
      Boundaries::exponential(0.0001, 2.0, 16),
    );
    let errors = Counter::new("id_effect_axum_tower_err_fail");
    let mut svc = EffectService::new((), |_env: &mut (), _x: u32| fail::<u32, &str, ()>("oops"))
      .with_request_metrics(latency.clone(), errors.clone());

    let out = svc.ready().await.unwrap().call(0).await;
    assert_eq!(out, Err(Cause::Fail("oops")));
    assert_eq!(errors.count(), 1);
    assert_eq!(latency.snapshot().count, 1);
  }

  #[tokio::test(flavor = "current_thread")]
  async fn effect_service_state_accessors() {
    #[derive(Clone, Default)]
    struct St {
      v: u32,
    }
    let mut svc: EffectService<St, _, u32> =
      EffectService::new(St { v: 10 }, |s: &mut St, _x: u32| {
        succeed::<u32, (), St>(s.v)
      });
    assert_eq!(svc.state().v, 10);
    svc.state_mut().v = 20;
    assert_eq!(svc.state().v, 20);
    assert_eq!(svc.ready().await.unwrap().call(0).await, Ok(20));
  }

  #[tokio::test(flavor = "current_thread")]
  async fn in_flight_counter_none_when_no_limit() {
    let svc: EffectService<(), _, u32> =
      EffectService::new((), |_env: &mut (), x: u32| succeed::<u32, (), ()>(x));
    assert!(svc.in_flight_counter().is_none());
  }

  #[tokio::test(flavor = "current_thread")]
  async fn in_flight_counter_released_when_the_call_is_dropped() {
    let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
    let started = Arc::new(std::sync::Mutex::new(Some(started_tx)));
    let mut svc = EffectService::with_max_in_flight((), 1_usize, move |_env: &mut (), _x: u32| {
      let started = started.clone();
      from_async(move || async move {
        if let Some(tx) = started.lock().unwrap().take() {
          let _ = tx.send(());
        }
        tokio::time::sleep(Duration::from_secs(3600)).await;
        Ok::<u32, ()>(0)
      })
    });
    let ctr = svc.in_flight_counter().expect("counter");
    svc.ready().await.unwrap();
    let call = tokio::spawn(svc.call(1));
    started_rx.await.unwrap();
    assert_eq!(read(&ctr).await, 1);
    call.abort();
    wait_for_zero(&ctr).await;
    // The permit came back too: the next call is admitted.
    tokio::time::timeout(Duration::from_secs(10), svc.ready())
      .await
      .expect("permit released")
      .unwrap();
  }

  #[tokio::test(flavor = "current_thread")]
  async fn a_panicking_handler_is_a_defect_and_releases_the_slot() {
    let mut svc = EffectService::with_max_in_flight((), 1_usize, |_env: &mut (), _x: u32| {
      sync::<u32, (), ()>(|| panic!("handler boom"))
    });
    let ctr = svc.in_flight_counter().expect("counter");
    svc.ready().await.unwrap();
    match svc.call(1).await {
      Err(Cause::Die(d)) => assert_eq!(d.message(), "handler boom"),
      other => panic!("expected a defect, got {other:?}"),
    }
    wait_for_zero(&ctr).await;
    tokio::time::timeout(Duration::from_secs(10), svc.ready())
      .await
      .expect("permit released")
      .unwrap();
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn service_futures_are_send_and_spawnable() {
    let mut svc = EffectService::with_max_in_flight((), 2_usize, |_env: &mut (), x: u32| {
      succeed::<u32, (), ()>(x * 3)
    });
    svc.ready().await.unwrap();
    let fut = svc.call(5);
    assert_eq!(tokio::spawn(fut).await.unwrap(), Ok(15));

    let mut chan = ChannelService::new(Channel::<u32>::duplex_unbounded());
    let fut = chan.ready().await.unwrap().call(9);
    assert_eq!(tokio::spawn(fut).await.unwrap(), Ok(9));
  }
}
