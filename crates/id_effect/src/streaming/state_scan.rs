//! FSM-style [`state_scan`] — step function returns optional outputs per element.
//!
//! Unlike [`Stream::scan`], which emits on every element, `state_scan` only yields when the
//! step function returns `Some(output)`.

use crate::streaming::stream::Stream;

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
  Stream::new(move |r: &mut R| {
    Box::pin(async move {
      let mut upstream = stream;
      let mut state = init;
      let mut out = Vec::new();
      loop {
        let Some(chunk) = upstream.poll_next_chunk(r).await? else {
          break;
        };
        for item in chunk.into_vec() {
          let (next, maybe) = step(state, item);
          state = next;
          if let Some(b) = maybe {
            out.push(b);
          }
        }
      }
      Ok(out)
    })
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
  fn empty_upstream_yields_empty() {
    let stream = Stream::<u8, (), ()>::from_iterable(core::iter::empty())
      .state_scan(0u32, |s, x| (s + x as u32, Some(s)));
    let out = block_on(stream.run_collect().run(&mut ())).expect("collect");
    assert!(out.is_empty());
  }
}
