//! FSM-style [`state_scan`] — step function returns optional outputs per element.
//!
//! Unlike [`Stream::scan`], which emits on every element, `state_scan` only yields when the
//! step function returns `Some(output)`.

use crate::streaming::stream::{StageStep, Stream};

/// Step function for [`state_scan`]: `(state, item) -> (next_state, optional_output)`.
pub type StateStep<S, A, B> = Box<dyn FnMut(S, A) -> (S, Option<B>) + Send>;

/// Stateful scan with optional emissions — useful for simple FSM transitions.
#[inline]
pub fn state_scan<S, A, B, E, R>(
  stream: Stream<A, E, R>,
  init: S,
  mut step: impl FnMut(S, A) -> (S, Option<B>) + Send + 'static,
) -> Stream<B, E, R>
where
  S: 'static,
  A: Send + 'static,
  B: Send + 'static,
  E: Send + 'static,
  R: 'static,
{
  // Incremental: one upstream chunk per pull; `state` is `None` only transiently inside `step`.
  let mut state = Some(init);
  stream.stage(move |input| {
    let mut out = Vec::new();
    for item in input.unwrap_or_default() {
      let (next, maybe) = step(state.take().expect("state_scan state present"), item);
      state = Some(next);
      out.extend(maybe);
    }
    StageStep::Emit(out)
  })
}

impl<A, E, R> Stream<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: 'static,
{
  /// Convenience wrapper around [`state_scan`] with the receiver as upstream.
  #[inline]
  pub fn state_scan<S, B, F>(self, init: S, step: F) -> Stream<B, E, R>
  where
    S: 'static,
    B: Send + 'static,
    F: FnMut(S, A) -> (S, Option<B>) + Send + 'static,
  {
    state_scan(self, init, step)
  }
}

#[cfg(test)]
mod tests {
  use super::state_scan;
  use super::*;

  fn block_on<F: core::future::Future>(fut: F) -> F::Output {
    pollster::block_on(fut)
  }

  #[derive(Clone, Copy, Debug, PartialEq, Eq)]
  enum St {
    Idle,
    Active,
  }

  #[test]
  fn emits_only_on_transition_to_active() {
    let stream = Stream::from_iterable([0u8, 1, 1, 0, 1]).state_scan(St::Idle, |st, x| {
      let next = if x > 0 { St::Active } else { St::Idle };
      let emit = matches!((st, next), (St::Idle, St::Active));
      (next, emit.then_some(x))
    });
    let out = block_on(stream.run_collect().run(&mut ())).expect("collect");
    assert_eq!(out, vec![1, 1]);
  }

  #[test]
  fn state_scan_no_transition_skips_emit() {
    let stream = Stream::from_iterable([1u8, 1, 1])
      .state_scan(0u8, |st, x| if x == st { (st, None) } else { (x, Some(x)) });
    let out = block_on(stream.run_collect().run(&mut ())).expect("collect");
    assert_eq!(out, vec![1]);
  }

  #[test]
  fn state_scan_emits_each_change() {
    let stream = Stream::from_iterable([1u8, 2, 3]).state_scan(0u8, |_st, x| (x, Some(x)));
    let out = block_on(stream.run_collect().run(&mut ())).expect("collect");
    assert_eq!(out, vec![1, 2, 3]);
  }

  #[test]
  fn free_state_scan_function() {
    let stream = state_scan(Stream::from_iterable([1u8, 2]), 0u32, |s, x| {
      (s + x as u32, Some(s))
    });
    let out = block_on(stream.run_collect().run(&mut ())).expect("collect");
    assert_eq!(out, vec![0, 1]);
  }

  #[test]
  fn state_scan_on_infinite_source_is_incremental() {
    // Emit the running sum every time it crosses another multiple of 10.
    let out = crate::testing::assert_bounded_memory(
      |s| {
        s.state_scan(0_u64, |sum, x| {
          let next = sum + x;
          (next, (next / 10 > sum / 10).then_some(next))
        })
      },
      3,
      64,
    );
    assert_eq!(out, vec![10, 21, 36]);
  }

  #[test]
  fn empty_upstream_yields_empty() {
    let stream = Stream::<u8, (), ()>::from_iterable(core::iter::empty())
      .state_scan(0u32, |s, x| (s + x as u32, Some(s)));
    let out = block_on(stream.run_collect().run(&mut ())).expect("collect");
    assert!(out.is_empty());
  }
}
