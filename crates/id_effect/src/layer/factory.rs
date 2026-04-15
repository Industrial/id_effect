//! [`Layer`] trait, constructors, and combinators (Stratum 5 — service factory API).
//!
//! Builds compile-time [`crate::context::Cons`] lists from parts ([`Layer`], [`Stack`]).
//! Pairs with [`crate::context::Context`]: run layers to obtain values, then wrap as
//! `Context(Cons(Tagged::<K,_>::new(v), …))`.

use crate::context::{Cons, Nil};
use crate::foundation::func::{compose, pipe1};
use crate::kernel::Effect;
use crate::runtime::{Never, run_blocking};
use std::rc::Rc;
use std::sync::Mutex;

/// Produces one heterogeneous cell (usually a [`Tagged`](crate::context::Tagged) or bare value you wrap yourself).
pub trait Layer {
  /// Value produced when [`Layer::build`] succeeds.
  type Output;
  /// Error type when [`Layer::build`] fails.
  type Error;
  /// Materializes this layer’s output (or error) synchronously.
  fn build(&self) -> Result<Self::Output, Self::Error>;
}

/// Construct an infallible layer from a clonable output value.
#[inline]
pub fn succeed<O: Clone>(output: O) -> LayerFn<impl Fn() -> Result<O, Never>> {
  LayerFn(move || Ok(output.clone()))
}

/// Construct a failing layer with a clonable error value.
#[inline]
pub fn fail<O, E: Clone>(error: E) -> LayerFn<impl Fn() -> Result<O, E>> {
  LayerFn(move || Err(error.clone()))
}

/// Construct a layer from a closure.
#[inline]
pub fn from_fn<O, E, F>(f: F) -> LayerFn<F>
where
  F: Fn() -> Result<O, E>,
{
  LayerFn(f)
}

/// Construct a layer from an effect and memoize its result.
///
/// The underlying effect is executed at most once; subsequent builds return the cached
/// success or failure value.
pub struct LayerEffect<O, E, R>
where
  O: Clone + 'static,
  E: Clone + 'static,
  R: Default + 'static,
{
  effect: Mutex<Option<Effect<O, E, R>>>,
  cached: Mutex<Option<Result<O, E>>>,
}

impl<O, E, R> LayerEffect<O, E, R>
where
  O: Clone + 'static,
  E: Clone + 'static,
  R: Default + 'static,
{
  /// Wraps `effect` so the first successful or failed run is cached for later [`Layer::build`] calls.
  #[inline]
  pub fn new(effect: Effect<O, E, R>) -> Self {
    Self {
      effect: Mutex::new(Some(effect)),
      cached: Mutex::new(None),
    }
  }
}

impl<O, E, R> Layer for LayerEffect<O, E, R>
where
  O: Clone + 'static,
  E: Clone + 'static,
  R: Default + 'static,
{
  type Output = O;
  type Error = E;

  fn build(&self) -> Result<Self::Output, Self::Error> {
    if let Some(result) = self
      .cached
      .lock()
      .expect("layer cache mutex poisoned")
      .clone()
    {
      return result;
    }

    let effect = self
      .effect
      .lock()
      .expect("layer effect mutex poisoned")
      .take()
      .expect("LayerEffect::build called after effect consumed without cache");
    let env = R::default();
    let result = run_blocking(effect, env);
    *self.cached.lock().expect("layer cache mutex poisoned") = Some(result.clone());
    result
  }
}

/// Constructor for [`LayerEffect`].
#[inline]
pub fn effect<O, E, R>(effect: Effect<O, E, R>) -> LayerEffect<O, E, R>
where
  O: Clone + 'static,
  E: Clone + 'static,
  R: Default + 'static,
{
  LayerEffect::new(effect)
}

/// Build from a zero-argument function (closure or fn pointer).
#[derive(Clone, Copy)]
pub struct LayerFn<F>(pub F);

impl<O, E, F> Layer for LayerFn<F>
where
  F: Fn() -> Result<O, E>,
{
  type Output = O;
  type Error = E;
  #[inline]
  fn build(&self) -> Result<O, E> {
    (self.0)()
  }
}

/// Run `A` then `B` (same error type) and pair outputs as [`Cons`].
#[derive(Clone, Copy)]
pub struct Stack<A, B>(pub A, pub B);

impl<A, B> Layer for Stack<A, B>
where
  A: Layer,
  B: Layer<Error = A::Error>,
{
  type Output = Cons<A::Output, Cons<B::Output, Nil>>;
  type Error = A::Error;
  #[inline]
  fn build(&self) -> Result<Self::Output, Self::Error> {
    pipe1(self.0.build(), |a| {
      a.and_then(|a| self.1.build().map(|b| Cons(a, Cons(b, Nil))))
    })
  }
}

/// Build an extra cell from a reference to the output of a previous layer.
pub trait LayerFrom<I: ?Sized> {
  /// Value produced from `input` when [`LayerFrom::build`] succeeds.
  type Output;
  /// Error type when [`LayerFrom::build`] fails.
  type Error;
  /// Builds using a shared borrow of the upstream layer’s output.
  fn build(&self, input: &I) -> Result<Self::Output, Self::Error>;
}

/// [`LayerFrom`] backed by a closure `Fn(&I) -> Result<O, E>`.
#[derive(Clone, Copy)]
pub struct LayerFnFrom<F>(pub F);

impl<I: ?Sized, O, E, F> LayerFrom<I> for LayerFnFrom<F>
where
  F: Fn(&I) -> Result<O, E>,
{
  type Output = O;
  type Error = E;
  #[inline]
  fn build(&self, input: &I) -> Result<O, E> {
    (self.0)(input)
  }
}

/// Run `A`, then run `B` with a borrow of `A`’s output; concatenate as [`Cons`].
#[derive(Clone, Copy)]
pub struct StackThen<A, B>(pub A, pub B);

impl<A, B> Layer for StackThen<A, B>
where
  A: Layer,
  B: LayerFrom<A::Output, Error = A::Error>,
{
  type Output = Cons<A::Output, Cons<B::Output, Nil>>;
  type Error = A::Error;
  #[inline]
  fn build(&self) -> Result<Self::Output, Self::Error> {
    pipe1(self.0.build(), |r| {
      r.and_then(|a| self.1.build(&a).map(|b| Cons(a, Cons(b, Nil))))
    })
  }
}

/// Extension combinators for transforming layer outputs and errors.
pub trait LayerExt: Layer + Sized {
  /// Maps successful output with `f` after [`Layer::build`].
  fn map<O2, F>(self, f: F) -> MapLayer<Self, F>
  where
    F: Fn(Self::Output) -> O2 + Clone,
  {
    MapLayer { layer: self, f }
  }

  /// Maps the error channel with `f` after [`Layer::build`].
  fn map_error<E2, F>(self, f: F) -> MapErrorLayer<Self, F>
  where
    F: Fn(Self::Error) -> E2 + Clone,
  {
    MapErrorLayer { layer: self, f }
  }

  /// Runs `self` and `that` and returns both outputs as a pair (fails on first error).
  fn merge<L2>(self, that: L2) -> MergeLayer<Self, L2>
  where
    L2: Layer<Error = Self::Error>,
  {
    MergeLayer {
      left: self,
      right: that,
    }
  }

  /// Runs `that` for side effects, discards its output, then runs `self`.
  fn provide<L0>(self, that: L0) -> ProvideLayer<Self, L0>
  where
    L0: Layer<Error = Self::Error>,
  {
    ProvideLayer {
      layer: self,
      provider: that,
    }
  }

  /// Runs `that` then `self`, returning `(self_output, provider_output)`.
  fn provide_merge<L0>(self, that: L0) -> ProvideMergeLayer<Self, L0>
  where
    L0: Layer<Error = Self::Error>,
  {
    ProvideMergeLayer {
      layer: self,
      provider: that,
    }
  }

  /// Marker wrapper; [`Layer::build`] delegates to the inner layer (for future scope/resource hooks).
  fn scoped(self) -> ScopedLayer<Self> {
    ScopedLayer { layer: self }
  }

  /// Turns this layer into an [`Effect`] that runs [`Layer::build`] and discards the output.
  fn launch(self) -> Effect<(), Self::Error, ()>
  where
    Self: 'static,
    Self::Error: 'static,
    Self::Output: 'static,
  {
    Effect::new(move |_env| self.build().map(|_| ()))
  }

  /// Feed `self` into a combinator (implemented with [`crate::foundation::func::pipe1`]).
  #[inline]
  fn pipe<O2, F>(self, f: F) -> O2
  where
    F: FnOnce(Self) -> O2,
  {
    pipe1(self, f)
  }

  /// Dependent stack: run `self`, then `next` with a borrow of the first output.
  /// Equivalent to [`StackThen`]`(self, next)`.
  #[inline]
  fn and_then<B>(self, next: B) -> StackThen<Self, B>
  where
    B: LayerFrom<Self::Output, Error = Self::Error>,
  {
    StackThen(self, next)
  }
}

impl<T> LayerExt for T where T: Layer {}

/// Layer that post-processes another layer’s successful output with `f`.
pub struct MapLayer<L, F> {
  layer: L,
  f: F,
}

impl<L, F, O2> Layer for MapLayer<L, F>
where
  L: Layer,
  F: Fn(L::Output) -> O2 + Clone,
{
  type Output = O2;
  type Error = L::Error;

  fn build(&self) -> Result<Self::Output, Self::Error> {
    self.layer.build().map(self.f.clone())
  }
}

/// Layer that maps another layer’s error with `f`.
pub struct MapErrorLayer<L, F> {
  layer: L,
  f: F,
}

impl<L, F, E2> Layer for MapErrorLayer<L, F>
where
  L: Layer,
  F: Fn(L::Error) -> E2 + Clone,
{
  type Output = L::Output;
  type Error = E2;

  fn build(&self) -> Result<Self::Output, Self::Error> {
    self.layer.build().map_err(self.f.clone())
  }
}

/// Runs two layers in sequence and pairs their outputs (same error type).
pub struct MergeLayer<A, B> {
  left: A,
  right: B,
}

impl<A, B> Layer for MergeLayer<A, B>
where
  A: Layer,
  B: Layer<Error = A::Error>,
{
  type Output = (A::Output, B::Output);
  type Error = A::Error;

  fn build(&self) -> Result<Self::Output, Self::Error> {
    Ok((self.left.build()?, self.right.build()?))
  }
}

/// Runs many layers of the same type and collects outputs in order (short-circuits on first error).
pub struct MergeAllLayer<L> {
  layers: Vec<L>,
}

impl<L> Layer for MergeAllLayer<L>
where
  L: Layer,
{
  type Output = Vec<L::Output>;
  type Error = L::Error;

  fn build(&self) -> Result<Self::Output, Self::Error> {
    type Acc<O, E> = Result<Vec<O>, E>;
    type ChainStep<'a, O, E> = Rc<dyn Fn(Acc<O, E>) -> Acc<O, E> + 'a>;
    let mut chain: ChainStep<'_, L::Output, L::Error> = Rc::new(|r| r);
    for layer in &self.layers {
      let prev = Rc::clone(&chain);
      chain = Rc::new(move |acc| {
        let prev = Rc::clone(&prev);
        compose(
          move |prev_acc: Acc<L::Output, L::Error>| {
            prev_acc.and_then(|mut v| {
              v.push(layer.build()?);
              Ok(v)
            })
          },
          move |x| prev(x),
        )(acc)
      });
    }
    chain(Ok(Vec::with_capacity(self.layers.len())))
  }
}

/// Builds a [`MergeAllLayer`] from an iterator of layers.
#[inline]
pub fn merge_all<L>(layers: impl IntoIterator<Item = L>) -> MergeAllLayer<L>
where
  L: Layer,
{
  MergeAllLayer {
    layers: layers.into_iter().collect(),
  }
}

/// Runs a provider layer first (output discarded), then the main layer.
pub struct ProvideLayer<L, P> {
  layer: L,
  provider: P,
}

impl<L, P> Layer for ProvideLayer<L, P>
where
  L: Layer,
  P: Layer<Error = L::Error>,
{
  type Output = L::Output;
  type Error = L::Error;

  fn build(&self) -> Result<Self::Output, Self::Error> {
    let _ = self.provider.build()?;
    self.layer.build()
  }
}

/// Runs provider then main layer and returns both outputs.
pub struct ProvideMergeLayer<L, P> {
  layer: L,
  provider: P,
}

impl<L, P> Layer for ProvideMergeLayer<L, P>
where
  L: Layer,
  P: Layer<Error = L::Error>,
{
  type Output = (L::Output, P::Output);
  type Error = L::Error;

  fn build(&self) -> Result<Self::Output, Self::Error> {
    let provided = self.provider.build()?;
    let output = self.layer.build()?;
    Ok((output, provided))
  }
}

/// Transparent wrapper around a layer (identity [`Layer::build`]).
pub struct ScopedLayer<L> {
  layer: L,
}

impl<L> Layer for ScopedLayer<L>
where
  L: Layer,
{
  type Output = L::Output;
  type Error = L::Error;

  fn build(&self) -> Result<Self::Output, Self::Error> {
    self.layer.build()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::context::{Context, Tagged, ThereHere};
  use rstest::rstest;

  struct DbKey;
  struct ClockKey;

  #[derive(Clone, Debug, PartialEq)]
  struct Boom;

  mod layer_fn {
    use super::*;

    #[test]
    fn build_returns_ok_when_closure_succeeds() {
      let layer = LayerFn(|| Ok::<i32, ()>(7));
      assert_eq!(layer.build(), Ok(7));
    }

    #[test]
    fn build_returns_err_when_closure_fails() {
      let layer: LayerFn<_> = LayerFn(|| Err::<i32, Boom>(Boom));
      assert_eq!(layer.build(), Err(Boom));
    }
  }

  mod constructors {
    use super::*;

    #[rstest]
    #[case::first_build(1u8)]
    #[case::second_build(2u8)]
    fn succeed_with_clonable_output_returns_ok_on_repeated_builds(#[case] value: u8) {
      let layer = super::succeed(value);
      assert_eq!(layer.build(), Ok(value));
      assert_eq!(layer.build(), Ok(value));
    }

    #[test]
    fn fail_with_clonable_error_returns_err_on_repeated_builds() {
      let layer = super::fail::<u8, _>(Boom);
      assert_eq!(layer.build(), Err(Boom));
      assert_eq!(layer.build(), Err(Boom));
    }

    #[test]
    fn effect_with_success_result_runs_underlying_effect_once_then_uses_cached_success() {
      let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
      let calls_ref = calls.clone();
      let layer = super::effect(Effect::new(move |_env: &mut ()| {
        calls_ref.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok::<u8, Boom>(9)
      }));

      assert_eq!(layer.build(), Ok(9));
      assert_eq!(layer.build(), Ok(9));
      assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn effect_with_failure_result_runs_underlying_effect_once_then_uses_cached_failure() {
      let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
      let calls_ref = calls.clone();
      let layer = super::effect(Effect::new(move |_env: &mut ()| {
        calls_ref.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err::<u8, Boom>(Boom)
      }));

      assert_eq!(layer.build(), Err(Boom));
      assert_eq!(layer.build(), Err(Boom));
      assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
  }

  mod layer_ext {
    use super::*;

    #[test]
    fn map_transforms_output_without_touching_error_channel() {
      let mapped = LayerFn(|| Ok::<u8, Boom>(3)).map(|n| n * 2);
      assert_eq!(mapped.build(), Ok(6));
    }

    #[test]
    fn map_error_transforms_error_without_touching_output() {
      let mapped = LayerFn(|| Err::<u8, i32>(4)).map_error(|n| n.to_string());
      assert_eq!(mapped.build(), Err(String::from("4")));
    }

    #[test]
    fn merge_with_two_successful_layers_returns_pair_of_outputs() {
      let merged = LayerFn(|| Ok::<u8, Boom>(1)).merge(LayerFn(|| Ok::<u16, Boom>(2)));
      assert_eq!(merged.build(), Ok((1, 2)));
    }

    #[test]
    fn merge_all_with_successful_layers_collects_outputs_in_layer_order() {
      let all = super::merge_all(vec![
        LayerFn((|| Ok::<u8, Boom>(3)) as fn() -> Result<u8, Boom>),
        LayerFn((|| Ok::<u8, Boom>(4)) as fn() -> Result<u8, Boom>),
      ]);
      assert_eq!(all.build(), Ok(vec![3, 4]));
    }

    #[test]
    #[allow(clippy::type_complexity)]
    fn merge_all_empty_returns_nil() {
      let all: MergeAllLayer<LayerFn<fn() -> Result<u8, Boom>>> = super::merge_all(vec![]);
      assert_eq!(all.build(), Ok(Vec::new()));
    }

    #[test]
    fn merge_all_two_layers_compose_order() {
      let all = super::merge_all(vec![
        LayerFn((|| Ok::<u32, Boom>(10u32)) as fn() -> Result<u32, Boom>),
        LayerFn((|| Ok::<u32, Boom>(20u32)) as fn() -> Result<u32, Boom>),
      ]);
      assert_eq!(all.build(), Ok(vec![10, 20]));
    }

    #[test]
    fn merge_all_with_failing_layer_returns_first_error() {
      let all = super::merge_all(vec![
        LayerFn((|| Ok::<u8, Boom>(3)) as fn() -> Result<u8, Boom>),
        LayerFn((|| Err::<u8, Boom>(Boom)) as fn() -> Result<u8, Boom>),
      ]);
      assert_eq!(all.build(), Err(Boom));
    }

    #[test]
    fn provide_with_successful_provider_builds_provider_then_returns_layer_output() {
      let provided = LayerFn(|| Ok::<u8, Boom>(7)).provide(LayerFn(|| Ok::<u8, Boom>(1)));
      assert_eq!(provided.build(), Ok(7));
    }

    #[test]
    fn provide_merge_with_successful_provider_returns_layer_output_and_provider_output() {
      let provided_merged =
        LayerFn(|| Ok::<u8, Boom>(9)).provide_merge(LayerFn(|| Ok::<u16, Boom>(5)));
      assert_eq!(provided_merged.build(), Ok((9, 5)));
    }

    #[test]
    fn provide_with_failing_provider_returns_provider_error_without_building_layer_output() {
      let provided = LayerFn(|| Ok::<u8, Boom>(7)).provide(LayerFn(|| Err::<u8, Boom>(Boom)));
      assert_eq!(provided.build(), Err(Boom));
    }

    #[test]
    fn scoped_wraps_layer_and_preserves_original_build_result() {
      let scoped = LayerFn(|| Ok::<u8, Boom>(6)).scoped();
      assert_eq!(scoped.build(), Ok(6));
    }

    #[test]
    fn launch_with_successful_layer_returns_unit_success_in_effect_runtime() {
      let launched = LayerFn(|| Ok::<u8, Boom>(1)).launch();
      assert_eq!(crate::runtime::run_blocking(launched, ()), Ok(()));
    }

    #[test]
    fn layer_ext_and_then_chains_two_layers() {
      let layer =
        LayerFn(|| Ok::<i32, ()>(3)).and_then(LayerFnFrom(|n: &i32| Ok::<i32, ()>(*n * 2)));
      let Cons(a, Cons(b, Nil)) = layer.build().unwrap();
      assert_eq!(a, 3);
      assert_eq!(b, 6);
    }

    #[test]
    fn layer_ext_pipe_passes_self_to_combinator() {
      let out = LayerFn(|| Ok::<u8, Boom>(2)).pipe(|l| l.map(|n| n + 1));
      assert_eq!(out.build(), Ok(3));
    }

    /// [`crate::foundation::func::flip`] helps when a two-argument helper’s parameter order is the
    /// opposite of how you want to supply arguments in a point-free test harness.
    #[test]
    fn flip_swaps_binary_function_argument_order() {
      use crate::foundation::func::flip;
      let sub = |a: i32, b: i32| a - b;
      assert_eq!(flip(sub)(3, 10), 7);
    }
  }

  mod stack {
    use super::*;

    #[test]
    fn build_pairs_outputs_when_both_layers_succeed() {
      let layer = Stack(
        LayerFn(|| Ok::<_, ()>(Tagged::<DbKey, _>::new(10i32))),
        LayerFn(|| Ok::<u64, ()>(99u64)),
      );
      let Cons(db, Cons(n, Nil)) = layer.build().unwrap();
      assert_eq!(db.value, 10);
      assert_eq!(n, 99u64);

      let ctx = Context::new(Cons(db, Cons(Tagged::<ClockKey, _>::new(n), Nil)));
      assert_eq!(*ctx.get::<DbKey>(), 10);
      assert_eq!(*ctx.get_path::<ClockKey, ThereHere>(), 99);
    }

    #[test]
    fn build_returns_err_when_first_layer_fails() {
      let layer = Stack(
        LayerFn(|| Err::<Tagged<DbKey, i32>, Boom>(Boom)),
        LayerFn(|| Ok::<u64, Boom>(0u64)),
      );
      assert!(matches!(layer.build(), Err(Boom)));
    }

    #[test]
    fn build_returns_err_when_second_layer_fails() {
      let layer = Stack(
        LayerFn(|| Ok::<Tagged<DbKey, i32>, Boom>(Tagged::<DbKey, _>::new(1))),
        LayerFn(|| Err::<u64, Boom>(Boom)),
      );
      assert!(matches!(layer.build(), Err(Boom)));
    }
  }

  mod stack_then {
    use super::*;

    #[test]
    fn build_passes_first_output_to_dependent_layer() {
      let layer = StackThen(
        LayerFn(|| Ok::<i32, ()>(3)),
        LayerFnFrom(|n: &i32| Ok::<i32, ()>(*n * 2)),
      );
      let Cons(a, Cons(b, Nil)) = layer.build().unwrap();
      assert_eq!(a, 3);
      assert_eq!(b, 6);
    }

    #[test]
    fn build_returns_err_when_first_layer_fails() {
      let layer = StackThen(
        LayerFn(|| Err::<i32, Boom>(Boom)),
        LayerFnFrom(|_: &i32| Ok::<i32, Boom>(0)),
      );
      assert_eq!(layer.build(), Err(Boom));
    }

    #[test]
    fn build_returns_err_when_dependent_layer_fails() {
      let layer = StackThen(
        LayerFn(|| Ok::<i32, Boom>(1)),
        LayerFnFrom(|_: &i32| Err::<i32, Boom>(Boom)),
      );
      assert_eq!(layer.build(), Err(Boom));
    }
  }

  mod layer_fn_from {
    use super::{Boom, LayerFnFrom, LayerFrom};
    use rstest::rstest;

    #[rstest]
    #[case::non_empty("abc", Ok(3))]
    #[case::empty("", Ok(0))]
    fn build_with_input_reference_invokes_closure_and_returns_expected_result(
      #[case] input: &'static str,
      #[case] expected: Result<usize, Boom>,
    ) {
      let layer = LayerFnFrom(|s: &str| Ok::<usize, ()>(s.len()));
      let actual = LayerFrom::build(&layer, input).map_err(|_| Boom);
      assert_eq!(actual, expected);
    }
  }
}
