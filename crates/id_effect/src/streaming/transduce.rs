//! Transducers on [`Stream`] ([`Stream::transduce_items`]) and the free [`state_scan`].
//!
//! [`Transducer`] itself is kernel-independent (`streaming::transducer`, re-exported by
//! `id_effect_optics`).

use super::stream::{StageStep, Stream};
use crate::streaming::transducer::Transducer;

/// Step function for [`state_scan`]: `(state, item) -> (next_state, optional_output)`.
pub type StateStep<S, A, B> = Box<dyn FnMut(S, A) -> (S, Option<B>) + Send>;

/// [`Stream::state_scan`] as a free function.
pub fn state_scan<S, A, B, E, R>(
  stream: Stream<A, E, R>,
  init: S,
  step: impl FnMut(S, A) -> (S, Option<B>) + Send + 'static,
) -> Stream<B, E, R>
where
  S: Send + 'static,
  A: Send + 'static,
  B: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  stream.state_scan(init, step)
}

impl<A, E, R> Stream<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  /// Applies `xf` to each element, emitting the transformed items in order (one upstream chunk
  /// per pull, each item run through the transducer on its own).
  pub fn transduce_items(self, xf: Transducer<A, Vec<A>>) -> Self {
    self.stage(move |input| {
      let mut out = Vec::new();
      for item in input.unwrap_or_default() {
        let push: crate::streaming::transducer::Reducer<A, Vec<A>> = Box::new(|mut acc, x| {
          acc.push(x);
          acc
        });
        out.extend(xf.transduce(std::iter::once(item), push, Vec::new()));
      }
      StageStep::Emit(out)
    })
  }

  /// Alias of [`transduce_items`](Self::transduce_items).
  pub fn via_transducer(self, xf: Transducer<A, Vec<A>>) -> Self {
    self.transduce_items(xf)
  }
}
