//! Parallel combinators through one choke point, `fork_all` (design A §4, the D6 Fabric hook),
//! plus `timeout` (a forked child raced against the executor timer).

use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use crate::cause::Cause;
use crate::effect::{Effect, fail, succeed, unbox};
use crate::fiber::{Cell, FiberContext, spawn_fiber};
use crate::runloop::{AnyBox, AnyExit, BoxFuture, Instr};

/// How `fork_all` schedules its children.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Concurrency {
  Sequential,
  /// At most `n` children at a time. Spike: run in waves of `n`, not a sliding window.
  Bounded(usize),
  Unbounded,
  /// Let the Fabric decide (D6). Spike: behaves as `Unbounded`; see [`Admission`].
  Inherit,
}

/// Fabric admission hook (D6). Spike: declared, not wired into `fork_all`.
pub trait Admission: Send + Sync {
  fn admit(&self, weight: u32) -> BoxFuture<'static, ()>;
}

type Kid = (Arc<FiberContext>, Cell);

/// Await every child; on the first failure interrupt the siblings, wait for them, and fail.
struct AllFut {
  kids: Vec<Kid>,
  results: Vec<Option<AnyBox>>,
  finished: Vec<bool>,
  failure: Option<Cause<AnyBox>>,
}

impl Future for AllFut {
  type Output = AnyExit;
  fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<AnyExit> {
    let this = &mut *self;
    let mut pending = 0;
    for i in 0..this.kids.len() {
      if this.finished[i] {
        continue;
      }
      match this.kids[i].1.poll_take(cx) {
        Poll::Ready(exit) => {
          this.finished[i] = true;
          match exit {
            Ok(v) => this.results[i] = Some(v),
            Err(c) => {
              if this.failure.is_none() {
                this.failure = Some(c);
                for (j, (kid, _)) in this.kids.iter().enumerate() {
                  if !this.finished[j] {
                    kid.request_interrupt();
                  }
                }
              }
            }
          }
        }
        Poll::Pending => pending += 1,
      }
    }
    if pending > 0 {
      return Poll::Pending;
    }
    if let Some(c) = this.failure.take() {
      return Poll::Ready(Err(c));
    }
    let vals: Vec<AnyBox> = this
      .results
      .iter_mut()
      .map(|r| r.take().expect("child result"))
      .collect();
    Poll::Ready(Ok(Box::new(vals)))
  }
}

impl Drop for AllFut {
  fn drop(&mut self) {
    for (i, (kid, _)) in self.kids.iter().enumerate() {
      if !self.finished[i] {
        kid.request_interrupt();
      }
    }
  }
}

fn fork_wave(instrs: Vec<Instr>) -> Instr {
  Instr::WithFiber(Box::new(move |env, fiber| {
    let kids: Vec<Kid> = instrs
      .into_iter()
      .map(|i| spawn_fiber(i, Arc::clone(env), Some(fiber), fiber.executor()))
      .collect();
    let n = kids.len();
    let fut = AllFut {
      kids,
      results: (0..n).map(|_| None).collect(),
      finished: vec![false; n],
      failure: None,
    };
    Instr::Async(Box::new(move |_, _| Box::pin(fut)))
  }))
}

/// Run every effect as a child fiber of the current one and collect the results in order.
/// The first failure interrupts the siblings (try-join, not join).
pub fn fork_all<A, E, R>(effects: Vec<Effect<A, E, R>>, c: Concurrency) -> Effect<Vec<A>, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  let unbox_all = |v: AnyBox| -> Vec<A> {
    unbox::<Vec<AnyBox>>(v)
      .into_iter()
      .map(unbox::<A>)
      .collect()
  };
  match c {
    Concurrency::Sequential => effects.into_iter().fold(succeed(Vec::new()), |acc, e| {
      acc.flat_map(move |mut v| {
        e.map(move |a| {
          v.push(a);
          v
        })
      })
    }),
    Concurrency::Unbounded | Concurrency::Inherit => {
      let instrs = effects.into_iter().map(Effect::into_instr).collect();
      Effect::from_instr(Instr::FlatMap(
        Box::new(fork_wave(instrs)),
        Box::new(move |v| Instr::Pure(Ok(Box::new(unbox_all(v))))),
      ))
    }
    Concurrency::Bounded(n) => {
      let n = n.max(1);
      let mut waves: Vec<Vec<Effect<A, E, R>>> = Vec::new();
      for e in effects {
        match waves.last_mut() {
          Some(w) if w.len() < n => w.push(e),
          _ => waves.push(vec![e]),
        }
      }
      waves.into_iter().fold(succeed(Vec::new()), |acc, wave| {
        acc.flat_map(move |mut v: Vec<A>| {
          fork_all(wave, Concurrency::Unbounded).map(move |w| {
            v.extend(w);
            v
          })
        })
      })
    }
  }
}

impl<A, E, R> Effect<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  /// Run both concurrently (two child fibers); the first failure interrupts the other.
  pub fn zip_par<B: Send + 'static>(self, other: Effect<B, E, R>) -> Effect<(A, B), E, R> {
    let instrs = vec![self.into_instr(), other.into_instr()];
    Effect::from_instr(Instr::FlatMap(
      Box::new(fork_wave(instrs)),
      Box::new(|v| {
        let mut it = unbox::<Vec<AnyBox>>(v).into_iter();
        let a = unbox::<A>(it.next().expect("zip_par left"));
        let b = unbox::<B>(it.next().expect("zip_par right"));
        Instr::Pure(Ok(Box::new((a, b))))
      }),
    ))
  }

  /// `None` if `d` elapses first; the effect is then interrupted (its finalizers run) and awaited.
  pub fn timeout(self, d: Duration) -> Effect<Option<A>, E, R> {
    Effect::from_instr(Instr::WithFiber(Box::new(move |env, fiber| {
      let kid = spawn_fiber(
        self.into_instr(),
        Arc::clone(env),
        Some(fiber),
        fiber.executor(),
      );
      let sleep = fiber.executor().sleep(d);
      Instr::Async(Box::new(move |_, _| {
        Box::pin(TimeoutFut::<A> {
          kid,
          sleep,
          timed_out: false,
          finished: false,
          _p: PhantomData,
        })
      }))
    })))
  }

  /// Fail with `on_timeout()` if `d` elapses first.
  pub fn timeout_fail(
    self,
    d: Duration,
    on_timeout: impl FnOnce() -> E + Send + 'static,
  ) -> Effect<A, E, R> {
    self.timeout(d).flat_map(move |o| match o {
      Some(a) => succeed(a),
      None => fail(on_timeout()),
    })
  }
}

struct TimeoutFut<A> {
  kid: Kid,
  sleep: BoxFuture<'static, ()>,
  timed_out: bool,
  finished: bool,
  _p: PhantomData<fn() -> A>,
}

impl<A: Send + 'static> Future for TimeoutFut<A> {
  type Output = AnyExit;
  fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<AnyExit> {
    let this = &mut *self;
    if !this.timed_out {
      if let Poll::Ready(exit) = this.kid.1.poll_take(cx) {
        this.finished = true;
        return Poll::Ready(exit.map(|v| Box::new(Some(unbox::<A>(v))) as AnyBox));
      }
      if this.sleep.as_mut().poll(cx).is_pending() {
        return Poll::Pending;
      }
      this.timed_out = true;
      this.kid.0.request_interrupt();
    }
    match this.kid.1.poll_take(cx) {
      Poll::Ready(_) => {
        this.finished = true;
        Poll::Ready(Ok(Box::new(None::<A>)))
      }
      Poll::Pending => Poll::Pending,
    }
  }
}

impl<A> Drop for TimeoutFut<A> {
  fn drop(&mut self) {
    if !self.finished {
      self.kid.0.request_interrupt();
    }
  }
}
