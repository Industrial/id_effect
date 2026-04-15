//! **Environment Optics** — `EnvLens`, `focus`, and compositional environment projection.
//!
//! In category theory a **lens** `Lens<S, A>` is a pair of functions:
//!
//! ```text
//! get : S → A
//! set : S × A → S    (or, for read-only lenses, omit set)
//! ```
//!
//! Applied to our effect system: `EnvLens<Outer, Inner>` expresses that the
//! `Inner` environment can be *projected from* `Outer`. This unlocks
//! **environment widening**: composing an `Effect<A, E, Inner>` into a larger
//! `Effect<A, E, Outer>` by focusing on the relevant slice.
//!
//! ## Relationship to zoom_env and local
//!
//! [`crate::kernel::effect::Effect::zoom_env`] is the *combinator* form — one
//! closure per call site. `EnvLens` is the *first-class value* form: lenses
//! can be stored, composed with `·`, and passed as arguments.
//!
//! ```text
//! zoom_env(f)(eff)     ≡     focus(EnvLens::new(f), eff)
//! ```
//!
//! ## Laws (van Laarhoven encoding, adapted for read-only)
//!
//! ```text
//! get (set s a) = a               -- GetPut  (trivially holds for Clone+project)
//! compose(l1, l2).get(s)
//!     = l2.get(l1.get(s))         -- Composition: (l1 · l2).get = l2.get ∘ l1.get
//! identity_lens().get(s) = s      -- Identity
//! ```
//!
//! For our projecting (read-only) variant `get` is pure; `set` is not provided
//! because the environment parameter is *contravariant* — we only project in.
//!
//! ## Haskell analogue
//!
//! ```haskell
//! type Lens s a = forall f. Functor f => (a -> f a) -> s -> f s
//! -- or, concretely, a record:
//! data Lens s a = Lens { get :: s -> a, set :: s -> a -> s }
//! ```
//!
//! Our `EnvLens<S, A>` corresponds to the read-only half `{ get :: s -> a }`,
//! which is all we need for contravariant environment composition.

use crate::kernel::Effect;

// ── EnvLens ───────────────────────────────────────────────────────────────────

/// A read-only lens from environment type `S` into sub-environment `A`.
///
/// Stores the projection as a boxed closure so lenses can be stored, named,
/// and composed as first-class values.
///
/// # Construction
///
/// ```rust
/// use id_effect::context::optics::EnvLens;
///
/// #[derive(Clone)]
/// struct App { port: u16, host: String }
///
/// // Lens from App to port
/// let port_lens: EnvLens<App, u16> = EnvLens::new(|a: &mut App| a.port);
/// ```
pub struct EnvLens<S, A> {
  project: Box<dyn FnOnce(&mut S) -> A + 'static>,
}

impl<S: 'static, A: 'static> EnvLens<S, A> {
  /// Create a lens from a projection closure.
  #[inline]
  pub fn new<F>(f: F) -> Self
  where
    F: FnOnce(&mut S) -> A + 'static,
  {
    Self {
      project: Box::new(f),
    }
  }

  /// Apply the lens projection to a value.
  #[inline]
  pub fn get(self, s: &mut S) -> A {
    (self.project)(s)
  }

  /// Compose two lenses: `self · other` = project `A` from `S` via `B`.
  ///
  /// ```text
  /// (self · other).get(s) = other.get(self.get(s))
  /// ```
  ///
  /// # Example
  ///
  /// ```rust
  /// use id_effect::context::optics::EnvLens;
  ///
  /// #[derive(Clone)] struct Outer { inner: Inner }
  /// #[derive(Clone)] struct Inner { value: i32 }
  ///
  /// let outer_to_inner: EnvLens<Outer, Inner> = EnvLens::new(|o: &mut Outer| o.inner.clone());
  /// let inner_to_val:   EnvLens<Inner, i32>   = EnvLens::new(|i: &mut Inner| i.value);
  ///
  /// let composed: EnvLens<Outer, i32> = outer_to_inner.compose(inner_to_val);
  ///
  /// let mut app = Outer { inner: Inner { value: 42 } };
  /// assert_eq!(composed.get(&mut app), 42);
  /// ```
  #[inline]
  pub fn compose<B: 'static>(self, other: EnvLens<A, B>) -> EnvLens<S, B> {
    EnvLens::new(move |s: &mut S| {
      let mut a = (self.project)(s);
      other.get(&mut a)
    })
  }
}

/// Identity lens: `S → S`.
///
/// Satisfies the identity law: `id_lens.get(s) = s`.
#[inline]
pub fn identity_lens<S: Clone + 'static>() -> EnvLens<S, S> {
  EnvLens::new(|s: &mut S| s.clone())
}

// ── focus: lens-based effect widening ────────────────────────────────────────

/// Run an `Effect<A, E, Inner>` inside an `Effect<A, E, Outer>` via a lens.
///
/// This is the *applicative* form of `zoom_env`: a lens value is passed
/// explicitly instead of an inline closure, allowing the lens to be named,
/// stored, and composed before use.
///
/// ```text
/// focus(lens, eff)(outer) = eff(lens.get(outer))
/// ```
///
/// ## Equivalence with zoom_env
///
/// ```rust,ignore
/// focus(EnvLens::new(f), eff)  ≡  eff.zoom_env(f)
/// ```
///
/// # Example
///
/// ```rust
/// use id_effect::context::optics::{EnvLens, focus};
/// use id_effect::kernel::effect::Effect;
/// use id_effect::runtime::run_blocking;
///
/// #[derive(Clone)]
/// struct AppEnv { value: i32 }
///
/// let inner_eff: Effect<i32, (), i32> = Effect::new(|n: &mut i32| Ok(*n * 2));
/// let lens = EnvLens::new(|a: &mut AppEnv| a.value);
///
/// let outer_eff = focus(lens, inner_eff);
/// assert_eq!(run_blocking(outer_eff, AppEnv { value: 21 }), Ok(42));
/// ```
#[inline]
pub fn focus<S, A, Err, Val>(lens: EnvLens<S, A>, eff: Effect<Val, Err, A>) -> Effect<Val, Err, S>
where
  S: 'static,
  A: 'static,
  Err: 'static,
  Val: 'static,
{
  eff.zoom_env(move |s: &mut S| lens.get(s))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
  use super::*;
  use crate::kernel::effect::Effect;
  use crate::runtime::run_blocking;
  use rstest::rstest;

  // ── Fixtures ─────────────────────────────────────────────────────────────

  #[derive(Clone, Debug)]
  struct App {
    multiplier: i32,
    offset: i32,
  }

  fn app(m: i32, o: i32) -> App {
    App {
      multiplier: m,
      offset: o,
    }
  }

  fn multiplier_lens() -> EnvLens<App, i32> {
    EnvLens::new(|a: &mut App| a.multiplier)
  }

  fn offset_lens() -> EnvLens<App, i32> {
    EnvLens::new(|a: &mut App| a.offset)
  }

  // inner effect: doubles its i32 environment
  fn double_inner() -> Effect<i32, (), i32> {
    Effect::new(|n: &mut i32| Ok(*n * 2))
  }

  // ── EnvLens::get ─────────────────────────────────────────────────────────

  mod env_lens_get {
    use super::*;

    #[rstest]
    #[case::mult(7, 3, 7)]
    #[case::off(7, 3, 3)]
    fn get_projects_the_correct_field(#[case] m: i32, #[case] o: i32, #[case] _expected: i32) {
      let mut a = app(m, o);
      assert_eq!(multiplier_lens().get(&mut a), m);
      let mut a2 = app(m, o);
      assert_eq!(offset_lens().get(&mut a2), o);
    }
  }

  // ── identity_lens ─────────────────────────────────────────────────────────

  mod identity_law {
    use super::*;

    /// `identity_lens.get(s) = s` — identity law.
    #[test]
    fn identity_lens_returns_value_unchanged() {
      let mut n = 42_i32;
      assert_eq!(identity_lens::<i32>().get(&mut n), 42);
    }

    /// `focus(identity_lens, eff)(env) = eff(env)` — focusing through identity is a no-op.
    #[test]
    fn focus_identity_lens_is_noop() {
      let eff: Effect<i32, (), i32> = Effect::new(|n: &mut i32| Ok(*n));
      let widened = focus(identity_lens::<i32>(), eff);
      assert_eq!(run_blocking(widened, 42), Ok(42));
    }
  }

  // ── composition law ────────────────────────────────────────────────────────

  mod composition_law {
    use super::*;

    #[derive(Clone)]
    struct Outer {
      inner: Inner,
    }
    #[derive(Clone)]
    struct Inner {
      value: i32,
    }

    fn outer_to_inner() -> EnvLens<Outer, Inner> {
      EnvLens::new(|o: &mut Outer| o.inner.clone())
    }

    fn inner_to_val() -> EnvLens<Inner, i32> {
      EnvLens::new(|i: &mut Inner| i.value)
    }

    /// `(l1 · l2).get(s) = l2.get(l1.get(s))` — composition law.
    #[test]
    fn composed_lens_projects_transitively() {
      let composed = outer_to_inner().compose(inner_to_val());
      let mut o = Outer {
        inner: Inner { value: 42 },
      };
      assert_eq!(composed.get(&mut o), 42);
    }

    /// Two-level composition with focus.
    #[test]
    fn focus_with_composed_lens() {
      let eff: Effect<i32, (), i32> = Effect::new(|n: &mut i32| Ok(*n + 1));
      let lens = outer_to_inner().compose(inner_to_val());
      let outer_eff = focus(lens, eff);
      let result = run_blocking(
        outer_eff,
        Outer {
          inner: Inner { value: 41 },
        },
      );
      assert_eq!(result, Ok(42));
    }
  }

  // ── focus ──────────────────────────────────────────────────────────────────

  mod focus_fn {
    use super::*;

    #[test]
    fn focus_runs_inner_effect_with_projected_env() {
      let outer_eff = focus(multiplier_lens(), double_inner());
      assert_eq!(run_blocking(outer_eff, app(21, 0)), Ok(42));
    }

    /// Different lenses on the same struct run independent inner effects.
    #[test]
    fn focus_with_different_lenses_independently() {
      let r1 = run_blocking(focus(multiplier_lens(), double_inner()), app(21, 0));
      let r2 = run_blocking(focus(offset_lens(), double_inner()), app(0, 21));
      assert_eq!(r1, Ok(42));
      assert_eq!(r2, Ok(42));
    }

    /// `focus(lens, eff)` is equivalent to `eff.zoom_env(|s| lens.get(s))`.
    #[test]
    fn focus_equivalent_to_zoom_env() {
      let r_focus = run_blocking(focus(multiplier_lens(), double_inner()), app(21, 0));
      let r_zoom = run_blocking(
        double_inner().zoom_env(|a: &mut App| a.multiplier),
        app(21, 0),
      );
      assert_eq!(r_focus, r_zoom);
    }
  }
}
