//! `Layer<Out, E, In>` (design A §2.3): an effect that builds the HList `Out` from `In`.
//! Spike scope: `succeed`, `from_effect`, `merge`, `provide_merge`, `build`. Not memoised, not
//! scoped (no finalizers), no `fresh()`.

use crate::effect::{Effect, succeed};
use crate::hlist::{Concat, Embed, HList};

#[must_use = "layers are lazy; build them with .build()"]
pub struct Layer<Out, E, In> {
  build: Effect<Out, E, In>,
}

impl<Out, E, In> Layer<Out, E, In>
where
  Out: HList,
  E: Send + 'static,
  In: HList,
{
  /// A layer from a ready environment value.
  pub fn succeed(out: Out) -> Self {
    Self {
      build: succeed(out),
    }
  }

  /// A layer whose construction is an effect over its input `In`.
  pub fn from_effect(build: Effect<Out, E, In>) -> Self {
    Self { build }
  }

  /// The construction effect.
  pub fn build(self) -> Effect<Out, E, In> {
    self.build
  }

  /// Side by side: outputs and inputs are concatenated. A capability needed by both inputs
  /// appears twice in the merged `In`, which makes the narrowing indices ambiguous (E0283):
  /// design A risk R1.
  pub fn merge<O2, I2, Is1, Is2>(
    self,
    other: Layer<O2, E, I2>,
  ) -> Layer<<Out as Concat<O2>>::Out, E, <In as Concat<I2>>::Out>
  where
    O2: HList,
    I2: HList,
    Out: Concat<O2>,
    In: Concat<I2>,
    <In as Concat<I2>>::Out: Embed<In, Is1> + Embed<I2, Is2>,
  {
    let left = self.build.narrow::<<In as Concat<I2>>::Out, Is1>();
    let right = other.build.narrow::<<In as Concat<I2>>::Out, Is2>();
    Layer {
      build: left.flat_map(move |o1| right.map(move |o2| o1.concat(o2))),
    }
  }

  /// Feed `base`'s output into this layer's input and keep both outputs (Effect.ts
  /// `Layer.provideMerge`): strata `base → self`.
  pub fn provide_merge<O0, I0, Is>(
    self,
    base: Layer<O0, E, I0>,
  ) -> Layer<<O0 as Concat<Out>>::Out, E, I0>
  where
    O0: HList + Concat<Out> + Embed<In, Is>,
    I0: HList,
  {
    let upper = self.build;
    Layer {
      build: base.build.flat_map(move |o0: O0| {
        upper
          .provide_env(o0.clone())
          .narrow::<I0, crate::hlist::HNil>()
          .map(move |out| o0.concat(out))
      }),
    }
  }
}
