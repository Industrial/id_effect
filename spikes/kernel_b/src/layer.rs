//! A minimal `Layer<Out, E, In>` (design §2.3 "Layer (D7) under B"): a constructor
//! `In -> Effect<Out, E, In>`. `and_then` feeds one layer's output in as the next one's env.
//! `merge` yields a **tuple**, not a type-level union: the user names the union with a derived
//! struct (`.map(|(a, b)| AppEnv { .. })`). That is the main parity loss against Effect.ts.
//!
//! Not spiked: memoization, `fresh`, `scoped` finalizers.

use crate::kernel::Effect;

pub struct Layer<Out, E, In = ()> {
  build: Box<dyn FnOnce() -> Effect<Out, E, In> + Send>,
}

impl<Out: Send + 'static, E: Send + 'static, In: Send + 'static> Layer<Out, E, In> {
  /// A layer from an effect that builds `Out` from the `In` environment.
  pub fn from_effect(eff: Effect<Out, E, In>) -> Self {
    Self {
      build: Box::new(move || eff),
    }
  }

  /// A layer from a constructor of the build effect.
  pub fn new<F: FnOnce() -> Effect<Out, E, In> + Send + 'static>(f: F) -> Self {
    Self { build: Box::new(f) }
  }

  /// The build effect.
  pub fn build(self) -> Effect<Out, E, In> {
    (self.build)()
  }

  /// Feed this layer's output in as `next`'s environment.
  pub fn and_then<Out2: Send + 'static>(self, next: Layer<Out2, E, Out>) -> Layer<Out2, E, In> {
    Layer::new(move || {
      self
        .build()
        .flat_map(move |out| next.build().provide(out).with_env())
    })
  }

  /// Both layers over the same input; the result is a tuple, not a union.
  pub fn merge<Out2: Send + 'static>(self, other: Layer<Out2, E, In>) -> Layer<(Out, Out2), E, In> {
    Layer::new(move || {
      self
        .build()
        .flat_map(move |a| other.build().map(move |b| (a, b)))
    })
  }

  pub fn map<O2: Send + 'static, F: FnOnce(Out) -> O2 + Send + 'static>(
    self,
    f: F,
  ) -> Layer<O2, E, In> {
    Layer::new(move || self.build().map(f))
  }
}

impl<E: Send + 'static, In: Clone + Send + 'static> Layer<In, E, In> {
  /// Pass the input environment through (to keep a lower stratum in the next one).
  pub fn identity() -> Self {
    Layer::new(|| Effect::sync(|r: &mut In| Ok(r.clone())))
  }
}
