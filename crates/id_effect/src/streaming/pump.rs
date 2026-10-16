//! Pumps: the machinery behind the operators that must wait on upstream *concurrently* with
//! something else (a timer for [`Stream::debounce`], other groups for [`Stream::group_by`]).
//!
//! A pump is a child fiber of the first pulling fiber that drives a producer into a bounded
//! [`Queue`] of chunk messages, so the consumer can wait on the queue and a timer at once without
//! interrupting (and so losing) an in-flight upstream pull. The bounded queue is the
//! backpressure: a full queue suspends the pump, which then stops pulling upstream.
//!
//! The pump is owned by a [`PumpGuard`] that travels with the consuming stream(s). Dropping the
//! last guard — the consumer stopped early, or was interrupted — interrupts the pump fiber (so
//! the finalizers of an in-flight upstream pull run and upstream is dropped) and shuts its
//! queues down. A pump is also interrupted when the fiber that started it exits.

use std::collections::VecDeque;
use std::future::{Future, poll_fn};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::Instant;

use super::stream::{Stream, lock};
use crate::coordination::Queue;
use crate::failure::Never;
use crate::kernel::{Ctx, Effect, Instr, erase, succeed, suspend, sync, yield_fiber};
use crate::runtime::FiberHandle;

/// One pump message: a non-empty chunk, or the upstream failure (the pump then stops). The end
/// of the stream is the shutdown of the queue.
pub(crate) type Msg<A, E> = Result<Vec<A>, E>;

/// Owns a pump fiber and closes its queues: dropping it interrupts the fiber (without waiting)
/// and runs `close`.
pub(crate) struct PumpGuard {
  fiber: Mutex<Option<FiberHandle<(), Never>>>,
  close: Box<dyn Fn() + Send + Sync>,
}

impl PumpGuard {
  pub(crate) fn new(close: impl Fn() + Send + Sync + 'static) -> Arc<Self> {
    Arc::new(Self {
      fiber: Mutex::new(None),
      close: Box::new(close),
    })
  }
}

impl Drop for PumpGuard {
  fn drop(&mut self) {
    if let Some(h) = lock(&self.fiber).take() {
      h.interrupt_now();
    }
    (self.close)();
  }
}

/// Forks `pump` (when given) as a child of the current fiber and hands its handle to `guard`.
pub(crate) fn start<E, R>(
  pump: Option<Effect<(), Never, R>>,
  guard: &Arc<PumpGuard>,
) -> Effect<(), E, R>
where
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  match pump {
    None => succeed(()),
    Some(pump) => {
      let guard = Arc::clone(guard);
      pump.fork().widen().map(move |h| {
        *lock(&guard.fiber) = Some(h);
      })
    }
  }
}

/// The current instant on the running fiber's executor clock (virtual time on a
/// [`TestExecutor`](crate::TestExecutor)).
pub(crate) fn fiber_now<E, R>() -> Effect<Instant, E, R>
where
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  Effect::from_instr(Instr::WithFiber(Box::new(|_, fiber| {
    Instr::Pure(Ok(erase(fiber.executor().now())))
  })))
}

/// The outcome of [`recv`].
pub(crate) enum Recv<A> {
  /// A chunk arrived.
  Chunk(Vec<A>),
  /// The queue is shut down and drained: the producer ended.
  End,
  /// The deadline passed first.
  Timeout,
}

/// Waits for the next message of `queue` until `deadline` (on the fiber's executor clock),
/// returning the outcome and the instant it was observed. A failure message fails the effect.
///
/// The take is cancel-safe: a message handed over but not observed (the timer won, or the
/// fiber was interrupted) goes back to the queue.
pub(crate) fn recv<A, E, R>(
  queue: Queue<Msg<A, E>>,
  deadline: Option<Instant>,
) -> Effect<(Recv<A>, Instant), E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  Effect::from_ctx(move |ctx: Ctx<R>| {
    let exec = Arc::clone(ctx.fiber().executor());
    let now = exec.now();
    let due = deadline.is_some_and(|d| d <= now);
    let mut timer = deadline
      .filter(|_| !due)
      .map(|d| exec.sleep(d.saturating_duration_since(now)));
    let mut take = (!due).then(|| queue.take_one_future());
    async move {
      let got = match take.as_mut() {
        None => None,
        Some(take) => {
          poll_fn(|cx| {
            if let Poll::Ready(r) = Pin::new(&mut *take).poll(cx) {
              return Poll::Ready(Some(r));
            }
            match timer.as_mut().map(|t| t.as_mut().poll(cx)) {
              Some(Poll::Ready(())) => Poll::Ready(None),
              _ => Poll::Pending,
            }
          })
          .await
        }
      };
      let out = match got {
        None => Recv::Timeout,
        Some(Ok(Ok(items))) => Recv::Chunk(items),
        Some(Ok(Err(e))) => return Err(e),
        Some(Err(_)) => Recv::End,
      };
      Ok((out, exec.now()))
    }
  })
}

/// Drives `upstream` into `out` one chunk of at most `n` items at a time, suspending while `out`
/// is full; stops once `out` refuses a chunk (it was shut down). A failure is sent as the last
/// message. Does not shut `out` down (see [`feed_pump`]).
pub(crate) fn feed<A, E, R>(
  upstream: Stream<A, E, R>,
  out: Queue<Msg<A, E>>,
  n: usize,
) -> Effect<(), Never, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  suspend(move || upstream.pull_raw(n))
    .map(Ok)
    .catch(|e| succeed(Err(e)))
    .flat_map(move |step| match step {
      Ok(None) => succeed(()),
      Ok(Some((items, rest))) if items.is_empty() => {
        yield_fiber().flat_map(move |()| feed(rest, out, n))
      }
      Ok(Some((items, rest))) => out.offer(Ok(items)).flat_map(move |stored| {
        if stored {
          feed(rest, out, n)
        } else {
          succeed(())
        }
      }),
      Err(e) => out.offer(Err(e)).map(|_| ()),
    })
}

/// [`feed`] that shuts `out` down when it ends, fails or is interrupted.
pub(crate) fn feed_pump<A, E, R>(
  upstream: Stream<A, E, R>,
  out: &Queue<Msg<A, E>>,
  n: usize,
) -> Effect<(), Never, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  let close = out.clone();
  feed(upstream, out.clone(), n).ensuring(sync(move || close.shutdown_sync()))
}

/// The stream of the chunks in `queue`: the first pull starts `pump`; it ends when the queue is
/// shut down and drained and fails with a failure message. Carries `guard`, so dropping the
/// stream stops the pump.
pub(crate) fn pumped<A, E, R>(
  pump: Option<Effect<(), Never, R>>,
  queue: Queue<Msg<A, E>>,
  guard: Arc<PumpGuard>,
  mut buf: VecDeque<A>,
) -> Stream<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  Stream::from_step(move |n| {
    if !buf.is_empty() {
      let out: Vec<A> = buf.drain(..n.min(buf.len())).collect();
      return succeed(Some((out, pumped(None, queue, guard, buf))));
    }
    start(pump, &guard).flat_map(move |()| {
      recv(queue.clone(), None).map(move |(got, _)| match got {
        Recv::Chunk(mut items) => {
          let rest: VecDeque<A> = if items.len() > n {
            items.split_off(n).into()
          } else {
            VecDeque::new()
          };
          Some((items, pumped(None, queue, guard, rest)))
        }
        Recv::End | Recv::Timeout => None,
      })
    })
  })
}

/// `stream`, carrying `guard` until it ends or is dropped.
#[allow(clippy::only_used_in_recursion)] // `guard` is only held: its `Drop` is the point
pub(crate) fn guarded<A, E, R, G>(stream: Stream<A, E, R>, guard: G) -> Stream<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
  G: Send + 'static,
{
  Stream::from_step(move |n| {
    stream
      .pull_raw(n)
      .map(move |step| step.map(|(items, rest)| (items, guarded(rest, guard))))
  })
}
