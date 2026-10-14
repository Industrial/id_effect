//! Higher-order function utilities — mirrors Effect.ts `Function` namespace.
//!
//! Provides the small functional building blocks core relies on (`identity`, `always`,
//! `compose`, `absurd`). The wider Effect.ts `Function` set (`const_`, `flip`, `pipe1`…`pipe3`,
//! `tupled`, `untupled`) lives in `id_effect_fp::foundation::function`.

/// `Function.identity` — returns its argument unchanged.
///
/// ```rust
/// use id_effect::foundation::func::identity;
/// assert_eq!(identity(42_i32), 42);
/// ```
pub fn identity<A>(a: A) -> A {
  a
}

/// `Function.constant` — the returned closure accepts a single argument of any type, ignores it
/// and returns `value`.
///
/// ```rust
/// use id_effect::foundation::func::always;
/// let always_true = always(true);
/// assert!(always_true(0_i32));
/// assert!(always_true(1_i32));
/// ```
pub fn always<A: Clone, B>(value: A) -> impl Fn(B) -> A + Clone {
  move |_| value.clone()
}

/// `Function.compose` — right-to-left function composition: `compose(f, g)(x) == f(g(x))`.
///
/// ```rust
/// use id_effect::foundation::func::compose;
/// let add_one = |n: i32| n + 1;
/// let double  = |n: i32| n * 2;
/// let double_then_inc = compose(add_one, double);
/// assert_eq!(double_then_inc(3), 7); // (3*2)+1
/// ```
pub fn compose<A, B, C>(f: impl Fn(B) -> C, g: impl Fn(A) -> B) -> impl Fn(A) -> C {
  move |a| f(g(a))
}

/// `Function.absurd` — a function whose input is the uninhabited `!` (never) type.
///
/// Call this in branches that are statically unreachable.
///
/// ```rust,ignore
/// use id_effect::foundation::func::absurd;
/// fn demo(x: std::convert::Infallible) -> i32 {
///     match x {}
/// }
/// ```
pub fn absurd<A>(never: std::convert::Infallible) -> A {
  match never {}
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
  use super::*;
  use rstest::rstest;

  // ── identity ──────────────────────────────────────────────────────────────

  mod identity_tests {
    use super::*;

    #[rstest]
    #[case::integer(42_i32)]
    #[case::zero(0_i32)]
    #[case::negative(-7_i32)]
    fn identity_returns_input(#[case] v: i32) {
      assert_eq!(identity(v), v);
    }

    #[test]
    fn identity_works_for_strings() {
      let s = "hello".to_string();
      assert_eq!(identity(s.clone()), s);
    }

    #[test]
    fn identity_works_for_options() {
      assert_eq!(identity(Some(1_i32)), Some(1));
    }
  }

  // ── always ────────────────────────────────────────────────────────────────

  mod always_tests {
    use super::*;

    #[test]
    fn always_ignores_argument() {
      let f = always::<bool, i32>(true);
      assert!(f(0));
      assert!(f(42));
    }

    #[test]
    fn always_with_string_value() {
      let f = always::<&str, i32>("fixed");
      assert_eq!(f(100), "fixed");
    }
  }

  // ── compose ───────────────────────────────────────────────────────────────

  mod compose_tests {
    use super::*;

    #[test]
    fn compose_applies_right_then_left() {
      let add_one = |n: i32| n + 1;
      let double = |n: i32| n * 2;
      let composed = compose(add_one, double);
      assert_eq!(composed(3), 7); // 3*2 then +1
    }

    #[test]
    fn compose_with_identity_is_identity() {
      let double = |n: i32| n * 2;
      let composed = compose(identity, double);
      assert_eq!(composed(5), 10);
    }

    #[rstest]
    #[case(0_i32, 1_i32)]
    #[case(1_i32, 3_i32)]
    #[case(2_i32, 5_i32)]
    fn compose_double_plus_one(#[case] input: i32, #[case] expected: i32) {
      let f = compose(|n: i32| n + 1, |n: i32| n * 2);
      assert_eq!(f(input), expected);
    }
  }

  // ── absurd ────────────────────────────────────────────────────────────────
  // (cannot construct Infallible in tests, but we can verify the signature compiles)
}
