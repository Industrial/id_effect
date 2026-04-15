//! Higher-order function utilities — mirrors Effect.ts `Function` namespace.
//!
//! Provides the functional building blocks (`identity`, `const_`, `flip`, `compose`,
//! `pipe`, `absurd`, `tupled`, `untupled`) that the rest of Effect.ts relies on to
//! express point-free style, lazy defaults, and function composition.

/// `Function.identity` — returns its argument unchanged.
///
/// ```rust
/// use id_effect::func::identity;
/// assert_eq!(identity(42_i32), 42);
/// ```
pub fn identity<A>(a: A) -> A {
  a
}

/// `Function.constFn` — returns a function that always returns `value`, ignoring its argument.
///
/// Named `const_` to avoid collision with the Rust `const` keyword.
///
/// ```rust
/// use id_effect::func::const_;
/// let always_five = const_(5_i32);
/// assert_eq!(always_five(()), 5);
/// ```
pub fn const_<A: Clone>(value: A) -> impl Fn(()) -> A + Clone {
  move |_| value.clone()
}

/// Like [`const_`] but the returned closure accepts a single argument of any type and ignores it.
///
/// ```rust
/// use id_effect::func::always;
/// let always_true = always(true);
/// assert!(always_true(0_i32));
/// assert!(always_true(1_i32));
/// ```
pub fn always<A: Clone, B>(value: A) -> impl Fn(B) -> A + Clone {
  move |_| value.clone()
}

/// `Function.flip` — swap the first two arguments of a two-argument function.
///
/// ```rust
/// use id_effect::func::flip;
/// let sub = |a: i32, b: i32| a - b;
/// let flipped = flip(sub);
/// assert_eq!(flipped(3, 10), 10 - 3);
/// ```
pub fn flip<A, B, C>(f: impl Fn(A, B) -> C) -> impl Fn(B, A) -> C {
  move |b, a| f(a, b)
}

/// `Function.compose` — right-to-left function composition: `compose(f, g)(x) == f(g(x))`.
///
/// ```rust
/// use id_effect::func::compose;
/// let add_one = |n: i32| n + 1;
/// let double  = |n: i32| n * 2;
/// let double_then_inc = compose(add_one, double);
/// assert_eq!(double_then_inc(3), 7); // (3*2)+1
/// ```
pub fn compose<A, B, C>(f: impl Fn(B) -> C, g: impl Fn(A) -> B) -> impl Fn(A) -> C {
  move |a| f(g(a))
}

/// `Function.pipe` (unary) — apply `f` to `a`.  Useful for point-free pipelines.
///
/// For multi-step pipelines prefer the [`pipe!`](macro@id_effect_macro::pipe) macro.
///
/// ```rust
/// use id_effect::func::pipe1;
/// assert_eq!(pipe1(5_i32, |n| n * 2), 10);
/// ```
pub fn pipe1<A, B>(a: A, f: impl FnOnce(A) -> B) -> B {
  f(a)
}

/// `Function.pipe` two steps.
pub fn pipe2<A, B, C>(a: A, f: impl FnOnce(A) -> B, g: impl FnOnce(B) -> C) -> C {
  g(f(a))
}

/// `Function.pipe` three steps.
pub fn pipe3<A, B, C, D>(
  a: A,
  f: impl FnOnce(A) -> B,
  g: impl FnOnce(B) -> C,
  h: impl FnOnce(C) -> D,
) -> D {
  h(g(f(a)))
}

/// `Function.absurd` — a function whose input is the uninhabited `!` (never) type.
///
/// Call this in branches that are statically unreachable.
///
/// ```rust,ignore
/// use id_effect::func::absurd;
/// fn demo(x: std::convert::Infallible) -> i32 {
///     match x {}
/// }
/// ```
pub fn absurd<A>(never: std::convert::Infallible) -> A {
  match never {}
}

/// `Function.tupled` — convert a two-argument function into a function that takes a tuple.
///
/// ```rust
/// use id_effect::func::tupled;
/// let add = |a: i32, b: i32| a + b;
/// let tupled_add = tupled(add);
/// assert_eq!(tupled_add((3, 4)), 7);
/// ```
pub fn tupled<A, B, C>(f: impl Fn(A, B) -> C) -> impl Fn((A, B)) -> C {
  move |(a, b)| f(a, b)
}

/// `Function.untupled` — convert a function that takes a tuple into a two-argument function.
///
/// ```rust
/// use id_effect::func::untupled;
/// let sum_pair = |(a, b): (i32, i32)| a + b;
/// let two_arg = untupled(sum_pair);
/// assert_eq!(two_arg(3, 4), 7);
/// ```
pub fn untupled<A, B, C>(f: impl Fn((A, B)) -> C) -> impl Fn(A, B) -> C {
  move |a, b| f((a, b))
}

/// Memoize a single-argument function with a `HashMap` cache (eagerly clones the key).
///
/// ```rust
/// use id_effect::func::memoize;
/// let mut double = memoize(|n: i32| n * 2);
/// assert_eq!(double(3), 6);
/// assert_eq!(double(3), 6); // cached
/// ```
pub fn memoize<A, B>(f: impl Fn(A) -> B) -> impl FnMut(A) -> B
where
  A: std::hash::Hash + Eq + Clone,
  B: Clone,
{
  let mut cache = std::collections::HashMap::<A, B>::new();
  move |a: A| {
    if let Some(v) = cache.get(&a) {
      return v.clone();
    }
    let v = f(a.clone());
    cache.insert(a, v.clone());
    v
  }
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

  // ── const_ / always ───────────────────────────────────────────────────────

  mod const_tests {
    use super::*;

    #[test]
    fn const_always_returns_same_value() {
      let f = const_(99_i32);
      assert_eq!(f(()), 99);
      assert_eq!(f(()), 99);
    }

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

  // ── flip ─────────────────────────────────────────────────────────────────

  mod flip_tests {
    use super::*;

    #[test]
    fn flip_reverses_arguments() {
      let sub = |a: i32, b: i32| a - b;
      let flipped = flip(sub);
      assert_eq!(flipped(3, 10), 10 - 3);
    }

    #[test]
    fn flip_of_flip_is_original() {
      let f = |a: i32, b: i32| a * 10 + b;
      // flip(flip(f)) behaves like f
      let result = flip(flip(|a: i32, b: i32| a * 10 + b))(1, 2);
      assert_eq!(result, f(1, 2));
    }

    #[rstest]
    #[case(10_i32, 3_i32, -7_i32)]
    #[case(5_i32, 5_i32, 0_i32)]
    #[case(0_i32, 1_i32, 1_i32)]
    fn flip_sub(#[case] b: i32, #[case] a: i32, #[case] expected: i32) {
      // flip(a - b)(b_arg, a_arg) == a_arg - b_arg
      let flipped_sub = flip(|a: i32, b: i32| a - b);
      assert_eq!(flipped_sub(b, a), expected);
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

  // ── pipe1 / pipe2 / pipe3 ─────────────────────────────────────────────────

  mod pipe_tests {
    use super::*;

    #[test]
    fn pipe1_applies_single_function() {
      assert_eq!(pipe1(5_i32, |n| n * 3), 15);
    }

    #[test]
    fn pipe2_applies_two_functions_left_to_right() {
      assert_eq!(pipe2(3_i32, |n| n * 2, |n| n + 1), 7);
    }

    #[test]
    fn pipe3_applies_three_functions_left_to_right() {
      assert_eq!(
        pipe3(2_i32, |n| n + 1, |n| n * 2, |n| n - 1),
        5 // (2+1)*2 - 1 = 5
      );
    }

    #[test]
    fn pipe1_with_string_conversion() {
      assert_eq!(pipe1(42_i32, |n| n.to_string()), "42");
    }
  }

  // ── absurd ────────────────────────────────────────────────────────────────
  // (cannot construct Infallible in tests, but we can verify the signature compiles)

  // ── tupled / untupled ─────────────────────────────────────────────────────

  mod tupled_tests {
    use super::*;

    #[test]
    fn tupled_converts_two_arg_to_tuple_arg() {
      let add = |a: i32, b: i32| a + b;
      let tupled_add = tupled(add);
      assert_eq!(tupled_add((3, 4)), 7);
    }

    #[test]
    fn untupled_converts_tuple_arg_to_two_arg() {
      let sum_pair = |(a, b): (i32, i32)| a + b;
      let two_arg = untupled(sum_pair);
      assert_eq!(two_arg(3, 4), 7);
    }

    #[test]
    fn tupled_then_untupled_is_original() {
      let f = |a: i32, b: i32| a - b;
      let roundtrip = untupled(tupled(f));
      assert_eq!(roundtrip(10, 3), 7);
    }

    #[rstest]
    #[case(1_i32, 2_i32, 3_i32)]
    #[case(0_i32, 0_i32, 0_i32)]
    #[case(-1_i32, 1_i32, 0_i32)]
    fn tupled_add_cases(#[case] a: i32, #[case] b: i32, #[case] expected: i32) {
      let f = tupled(|a: i32, b: i32| a + b);
      assert_eq!(f((a, b)), expected);
    }
  }

  // ── memoize ───────────────────────────────────────────────────────────────

  mod memoize_tests {
    use super::*;

    #[test]
    fn memoize_returns_correct_value() {
      let double = memoize(|n: i32| n * 2);
      // (memoize returns FnMut, bind to mut)
      let mut double = double;
      assert_eq!(double(5), 10);
    }

    #[test]
    fn memoize_returns_same_value_on_second_call() {
      let mut f = memoize(|n: i32| n + 100);
      assert_eq!(f(3), 103);
      assert_eq!(f(3), 103);
    }

    #[test]
    fn memoize_caches_independently_per_key() {
      let mut f = memoize(|s: &str| s.len());
      assert_eq!(f("hi"), 2);
      assert_eq!(f("hello"), 5);
      assert_eq!(f("hi"), 2);
    }
  }
}
