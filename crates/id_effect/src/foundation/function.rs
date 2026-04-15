//! **Functions** — morphisms in the category of types.
//!
//! Functions are the arrows (morphisms) that connect types. This module provides
//! the fundamental function combinators that form the basis of functional programming.
//!
//! ## Core Combinators
//!
//! | Function | Signature | Description |
//! |----------|-----------|-------------|
//! | [`identity`] | `A → A` | Returns input unchanged |
//! | [`const_`] | `A → (() → A)` | Ignores input, returns constant |
//! | [`always`] | `A → (B → A)` | Ignores any input, returns constant |
//! | [`compose`] | `(B → C) → (A → B) → (A → C)` | Right-to-left composition |
//! | [`flip`] | `((A, B) → C) → ((B, A) → C)` | Swap arguments |
//! | [`absurd`] | `Never → A` | Ex falso quodlibet |
//!
//! ## Laws
//!
//! - **Left identity**: `compose(identity, f) ≡ f`
//! - **Right identity**: `compose(f, identity) ≡ f`
//! - **Associativity**: `compose(f, compose(g, h)) ≡ compose(compose(f, g), h)`
//! - **Flip involution**: `flip(flip(f)) ≡ f`

use core::convert::Infallible;

/// The identity function — returns its argument unchanged.
///
/// This is the identity morphism for every object in the category of types.
/// It satisfies: `identity(x) ≡ x` for all `x`.
///
/// # Examples
///
/// ```rust
/// use id_effect::foundation::function::identity;
///
/// assert_eq!(identity(42), 42);
/// assert_eq!(identity("hello"), "hello");
/// ```
#[inline]
pub fn identity<A>(a: A) -> A {
  a
}

/// Create a constant function that ignores unit and returns `value`.
///
/// Named `const_` to avoid collision with the Rust `const` keyword.
///
/// # Examples
///
/// ```rust
/// use id_effect::foundation::function::const_;
///
/// let always_five = const_(5);
/// assert_eq!(always_five(()), 5);
/// ```
#[inline]
pub fn const_<A: Clone>(value: A) -> impl Fn(()) -> A + Clone {
  move |()| value.clone()
}

/// Create a constant function that ignores any argument and returns `value`.
///
/// This is a generalization of [`const_`] that works with any input type.
///
/// # Examples
///
/// ```rust
/// use id_effect::foundation::function::always;
///
/// let always_true = always::<bool, i32>(true);
/// assert!(always_true(0));
/// assert!(always_true(1));
/// ```
#[inline]
pub fn always<A: Clone, B>(value: A) -> impl Fn(B) -> A + Clone {
  move |_| value.clone()
}

/// Swap the arguments of a two-argument function.
///
/// `flip(f)(b, a) ≡ f(a, b)`
///
/// # Laws
///
/// - **Involution**: `flip(flip(f)) ≡ f`
///
/// # Examples
///
/// ```rust
/// use id_effect::foundation::function::flip;
///
/// let sub = |a: i32, b: i32| a - b;
/// let flipped = flip(sub);
/// assert_eq!(flipped(3, 10), 7); // 10 - 3
/// ```
#[inline]
pub fn flip<A, B, C>(f: impl Fn(A, B) -> C) -> impl Fn(B, A) -> C {
  move |b, a| f(a, b)
}

/// Right-to-left function composition.
///
/// `compose(f, g)(x) ≡ f(g(x))`
///
/// # Laws
///
/// - **Left identity**: `compose(identity, f) ≡ f`
/// - **Right identity**: `compose(f, identity) ≡ f`
/// - **Associativity**: `compose(f, compose(g, h)) ≡ compose(compose(f, g), h)`
///
/// # Examples
///
/// ```rust
/// use id_effect::foundation::function::compose;
///
/// let add_one = |n: i32| n + 1;
/// let double = |n: i32| n * 2;
/// let double_then_add = compose(add_one, double);
/// assert_eq!(double_then_add(3), 7); // (3 * 2) + 1
/// ```
#[inline]
pub fn compose<A, B, C>(f: impl Fn(B) -> C, g: impl Fn(A) -> B) -> impl Fn(A) -> C {
  move |a| f(g(a))
}

/// Left-to-right function composition (flip of compose).
///
/// `and_then(f, g)(x) ≡ g(f(x))`
///
/// # Examples
///
/// ```rust
/// use id_effect::foundation::function::and_then;
///
/// let add_one = |n: i32| n + 1;
/// let double = |n: i32| n * 2;
/// let add_then_double = and_then(add_one, double);
/// assert_eq!(add_then_double(3), 8); // (3 + 1) * 2
/// ```
#[inline]
pub fn and_then<A, B, C>(f: impl Fn(A) -> B, g: impl Fn(B) -> C) -> impl Fn(A) -> C {
  move |a| g(f(a))
}

/// Eliminate the never type by producing any value.
///
/// Since the never type has no inhabitants, this function can never be called.
/// It exists to satisfy the type checker in impossible branches.
///
/// This is the unique morphism from the initial object: `absurd : 0 → A`
///
/// # Examples
///
/// ```rust,ignore
/// use id_effect::foundation::function::absurd;
///
/// fn handle_result(r: Result<i32, std::convert::Infallible>) -> i32 {
///     match r {
///         Ok(v) => v,
///         Err(never) => absurd(never),
///     }
/// }
/// ```
#[inline]
pub fn absurd<A>(never: Infallible) -> A {
  match never {}
}

/// Apply a single function to a value (unary pipe).
///
/// `pipe1(x, f) ≡ f(x)`
///
/// # Examples
///
/// ```rust
/// use id_effect::foundation::function::pipe1;
///
/// assert_eq!(pipe1(5, |n| n * 2), 10);
/// ```
#[inline]
pub fn pipe1<A, B>(a: A, f: impl FnOnce(A) -> B) -> B {
  f(a)
}

/// Apply two functions in sequence (binary pipe).
///
/// `pipe2(x, f, g) ≡ g(f(x))`
///
/// # Examples
///
/// ```rust
/// use id_effect::foundation::function::pipe2;
///
/// assert_eq!(pipe2(3, |n| n * 2, |n| n + 1), 7);
/// ```
#[inline]
pub fn pipe2<A, B, C>(a: A, f: impl FnOnce(A) -> B, g: impl FnOnce(B) -> C) -> C {
  g(f(a))
}

/// Apply three functions in sequence (ternary pipe).
///
/// `pipe3(x, f, g, h) ≡ h(g(f(x)))`
///
/// # Examples
///
/// ```rust
/// use id_effect::foundation::function::pipe3;
///
/// assert_eq!(pipe3(2, |n| n + 1, |n| n * 2, |n| n - 1), 5);
/// ```
#[inline]
pub fn pipe3<A, B, C, D>(
  a: A,
  f: impl FnOnce(A) -> B,
  g: impl FnOnce(B) -> C,
  h: impl FnOnce(C) -> D,
) -> D {
  h(g(f(a)))
}

/// Convert a two-argument function to accept a tuple.
///
/// `tupled(f)((a, b)) ≡ f(a, b)`
///
/// # Examples
///
/// ```rust
/// use id_effect::foundation::function::tupled;
///
/// let add = |a: i32, b: i32| a + b;
/// let tupled_add = tupled(add);
/// assert_eq!(tupled_add((3, 4)), 7);
/// ```
#[inline]
pub fn tupled<A, B, C>(f: impl Fn(A, B) -> C) -> impl Fn((A, B)) -> C {
  move |(a, b)| f(a, b)
}

/// Convert a tuple-accepting function to take two arguments.
///
/// `untupled(f)(a, b) ≡ f((a, b))`
///
/// This is the inverse of [`tupled`].
///
/// # Examples
///
/// ```rust
/// use id_effect::foundation::function::untupled;
///
/// let sum_pair = |(a, b): (i32, i32)| a + b;
/// let two_arg = untupled(sum_pair);
/// assert_eq!(two_arg(3, 4), 7);
/// ```
#[inline]
pub fn untupled<A, B, C>(f: impl Fn((A, B)) -> C) -> impl Fn(A, B) -> C {
  move |a, b| f((a, b))
}

#[cfg(test)]
mod tests {
  use super::*;
  use rstest::rstest;

  mod identity_tests {
    use super::*;

    #[rstest]
    #[case::integer(42_i32)]
    #[case::zero(0_i32)]
    #[case::negative(-7_i32)]
    fn identity_returns_input_unchanged(#[case] value: i32) {
      assert_eq!(identity(value), value);
    }

    #[test]
    fn identity_preserves_string() {
      let s = String::from("hello");
      assert_eq!(identity(s.clone()), s);
    }

    #[test]
    fn identity_preserves_option() {
      assert_eq!(identity(Some(1_i32)), Some(1));
      assert_eq!(identity(None::<i32>), None);
    }
  }

  mod const_tests {
    use super::*;

    #[test]
    fn const_returns_same_value_on_repeated_calls() {
      let f = const_(99_i32);
      assert_eq!(f(()), 99);
      assert_eq!(f(()), 99);
    }

    #[test]
    fn const_with_string() {
      let f = const_(String::from("fixed"));
      assert_eq!(f(()), "fixed");
    }
  }

  mod always_tests {
    use super::*;

    #[rstest]
    #[case::ignore_int(0_i32)]
    #[case::ignore_another_int(42_i32)]
    #[case::ignore_negative(-1_i32)]
    fn always_ignores_argument(#[case] ignored: i32) {
      let f = always::<bool, i32>(true);
      assert!(f(ignored));
    }

    #[test]
    fn always_with_string_value() {
      let f = always::<&str, i32>("constant");
      assert_eq!(f(100), "constant");
    }
  }

  mod flip_tests {
    use super::*;

    #[test]
    fn flip_reverses_arguments() {
      let sub = |a: i32, b: i32| a - b;
      let flipped = flip(sub);
      assert_eq!(flipped(3, 10), 7); // 10 - 3
    }

    #[test]
    fn flip_involution_law() {
      let f = |a: i32, b: i32| a * 10 + b;
      // flip(flip(f)) should behave like f
      let double_flipped = flip(flip(|a: i32, b: i32| a * 10 + b));
      assert_eq!(double_flipped(1, 2), f(1, 2));
    }

    #[rstest]
    #[case::a_gt_b(10_i32, 3_i32, -7_i32)] // flip(sub)(10, 3) = sub(3, 10) = -7
    #[case::a_eq_b(5_i32, 5_i32, 0_i32)] // flip(sub)(5, 5) = sub(5, 5) = 0
    #[case::a_lt_b(0_i32, 1_i32, 1_i32)] // flip(sub)(0, 1) = sub(1, 0) = 1
    fn flip_sub_cases(#[case] first: i32, #[case] second: i32, #[case] expected: i32) {
      // flip(f)(x, y) = f(y, x), so flip(sub)(first, second) = sub(second, first) = second - first
      let flipped_sub = flip(|a: i32, b: i32| a - b);
      assert_eq!(flipped_sub(first, second), expected);
    }
  }

  mod compose_tests {
    use super::*;

    #[test]
    fn compose_applies_right_then_left() {
      let add_one = |n: i32| n + 1;
      let double = |n: i32| n * 2;
      let composed = compose(add_one, double);
      assert_eq!(composed(3), 7); // (3*2) + 1
    }

    #[test]
    fn compose_left_identity_law() {
      let double = |n: i32| n * 2;
      let composed = compose(identity, double);
      assert_eq!(composed(5), double(5));
    }

    #[test]
    fn compose_right_identity_law() {
      let double = |n: i32| n * 2;
      let composed = compose(double, identity);
      assert_eq!(composed(5), double(5));
    }

    #[rstest]
    #[case::zero(0_i32, 1_i32)]
    #[case::one(1_i32, 3_i32)]
    #[case::two(2_i32, 5_i32)]
    fn compose_double_plus_one(#[case] input: i32, #[case] expected: i32) {
      let f = compose(|n: i32| n + 1, |n: i32| n * 2);
      assert_eq!(f(input), expected);
    }
  }

  mod and_then_tests {
    use super::*;

    #[test]
    fn and_then_applies_left_to_right() {
      let add_one = |n: i32| n + 1;
      let double = |n: i32| n * 2;
      let chained = and_then(add_one, double);
      assert_eq!(chained(3), 8); // (3+1) * 2
    }

    #[test]
    fn and_then_is_flip_of_compose() {
      let f = |n: i32| n + 1;
      let g = |n: i32| n * 2;
      let via_and_then = and_then(f, g);
      let via_compose = compose(g, f);
      assert_eq!(via_and_then(5), via_compose(5));
    }
  }

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
      assert_eq!(pipe3(2_i32, |n| n + 1, |n| n * 2, |n| n - 1), 5);
    }

    #[test]
    fn pipe1_with_type_conversion() {
      assert_eq!(pipe1(42_i32, |n| n.to_string()), "42");
    }
  }

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
    fn tupled_then_untupled_is_identity() {
      let f = |a: i32, b: i32| a - b;
      let roundtrip = untupled(tupled(f));
      assert_eq!(roundtrip(10, 3), f(10, 3));
    }

    #[test]
    fn untupled_then_tupled_is_identity() {
      let f = |(a, b): (i32, i32)| a - b;
      let roundtrip = tupled(untupled(f));
      assert_eq!(roundtrip((10, 3)), f((10, 3)));
    }

    #[rstest]
    #[case::positive((1_i32, 2_i32), 3_i32)]
    #[case::zero((0_i32, 0_i32), 0_i32)]
    #[case::negative((-1_i32, 1_i32), 0_i32)]
    fn tupled_add_cases(#[case] pair: (i32, i32), #[case] expected: i32) {
      let f = tupled(|a: i32, b: i32| a + b);
      assert_eq!(f(pair), expected);
    }
  }

  mod absurd_tests {
    use super::*;

    #[test]
    fn absurd_in_result_match() {
      let result: Result<i32, Infallible> = Ok(42);
      let value = match result {
        Ok(v) => v,
        Err(never) => absurd(never),
      };
      assert_eq!(value, 42);
    }

    #[test]
    fn absurd_signature_is_polymorphic() {
      fn _to_int(n: Infallible) -> i32 {
        absurd(n)
      }
      fn _to_string(n: Infallible) -> String {
        absurd(n)
      }
      fn _to_unit(n: Infallible) {
        absurd(n)
      }
    }
  }

  mod laws {
    use super::*;

    #[test]
    fn compose_associativity() {
      let f = |n: i32| n + 1;
      let g = |n: i32| n * 2;
      let h = |n: i32| n - 3;

      // compose(f, compose(g, h)) ≡ compose(compose(f, g), h)
      let left = compose(f, compose(g, h));
      let right = compose(compose(f, g), h);

      for x in [-10, 0, 5, 100] {
        assert_eq!(left(x), right(x), "associativity failed for x={x}");
      }
    }

    #[test]
    fn identity_is_neutral_element() {
      let f = |n: i32| n * 2 + 1;

      for x in [-5, 0, 7, 42] {
        assert_eq!(compose(identity, f)(x), f(x), "left identity failed");
        assert_eq!(compose(f, identity)(x), f(x), "right identity failed");
      }
    }
  }
}
