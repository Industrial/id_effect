//! **Product** — the categorical product of types.
//!
//! The product of two types `A` and `B` is a type `(A, B)` together with
//! projection functions `fst` and `snd`. In category theory, products satisfy
//! a universal property: for any type `C` with functions `f: C → A` and `g: C → B`,
//! there exists a unique function `h: C → (A, B)` such that `fst ∘ h = f` and `snd ∘ h = g`.
//!
//! ## Properties
//!
//! - **Projections**: `fst((a, b)) = a`, `snd((a, b)) = b`
//! - **Pairing**: `(fst(p), snd(p)) = p`
//! - **Commutativity**: `(A, B) ≅ (B, A)` via [`swap`]
//! - **Associativity**: `((A, B), C) ≅ (A, (B, C))`
//! - **Unit identity**: `(A, ()) ≅ A ≅ ((), A)`
//!
//! ## Combinators
//!
//! | Function | Signature | Description |
//! |----------|-----------|-------------|
//! | [`fst`] | `(A, B) → A` | First projection |
//! | [`snd`] | `(A, B) → B` | Second projection |
//! | [`pair`] | `(A → B) → (A → C) → (A → (B, C))` | Diagonal/fanout |
//! | [`bimap_product`] | `(A → C) → (B → D) → ((A, B) → (C, D))` | Map both components |
//! | [`swap`] | `(A, B) → (B, A)` | Swap components |

/// Extract the first component of a pair.
///
/// This is the first projection morphism of the categorical product.
///
/// # Examples
///
/// ```rust
/// use id_effect::foundation::product::fst;
///
/// assert_eq!(fst((1, "hello")), 1);
/// assert_eq!(fst(("a", "b")), "a");
/// ```
#[inline]
pub fn fst<A, B>(pair: (A, B)) -> A {
  pair.0
}

/// Extract the second component of a pair.
///
/// This is the second projection morphism of the categorical product.
///
/// # Examples
///
/// ```rust
/// use id_effect::foundation::product::snd;
///
/// assert_eq!(snd((1, "hello")), "hello");
/// assert_eq!(snd(("a", "b")), "b");
/// ```
#[inline]
pub fn snd<A, B>(pair: (A, B)) -> B {
  pair.1
}

/// Create the diagonal/fanout combinator.
///
/// Given functions `f: A → B` and `g: A → C`, produce a function `A → (B, C)`
/// that applies both to the same input.
///
/// This witnesses the universal property of products.
///
/// # Examples
///
/// ```rust
/// use id_effect::foundation::product::pair;
///
/// let double = |n: i32| n * 2;
/// let negate = |n: i32| -n;
/// let both = pair(double, negate);
///
/// assert_eq!(both(5), (10, -5));
/// ```
#[inline]
pub fn pair<A, B, C>(f: impl Fn(A) -> B, g: impl Fn(A) -> C) -> impl Fn(A) -> (B, C)
where
  A: Clone,
{
  move |a: A| (f(a.clone()), g(a))
}

/// Map both components of a pair independently.
///
/// `bimap_product(f, g)((a, b)) = (f(a), g(b))`
///
/// This is the bifunctor action on products.
///
/// # Examples
///
/// ```rust
/// use id_effect::foundation::product::bimap_product;
///
/// let double = |n: i32| n * 2;
/// let len = |s: &str| s.len();
/// let transform = bimap_product(double, len);
///
/// assert_eq!(transform((5, "hello")), (10, 5));
/// ```
#[inline]
pub fn bimap_product<A, B, C, D>(
  f: impl Fn(A) -> C,
  g: impl Fn(B) -> D,
) -> impl Fn((A, B)) -> (C, D) {
  move |(a, b)| (f(a), g(b))
}

/// Map the first component of a pair.
///
/// `map_fst(f)((a, b)) = (f(a), b)`
///
/// # Examples
///
/// ```rust
/// use id_effect::foundation::product::map_fst;
///
/// let double = |n: i32| n * 2;
/// let transform = map_fst(double);
///
/// assert_eq!(transform((5, "hello")), (10, "hello"));
/// ```
#[inline]
pub fn map_fst<A, B, C>(f: impl Fn(A) -> C) -> impl Fn((A, B)) -> (C, B) {
  move |(a, b)| (f(a), b)
}

/// Map the second component of a pair.
///
/// `map_snd(f)((a, b)) = (a, f(b))`
///
/// # Examples
///
/// ```rust
/// use id_effect::foundation::product::map_snd;
///
/// let len = |s: &str| s.len();
/// let transform = map_snd(len);
///
/// assert_eq!(transform((5, "hello")), (5, 5));
/// ```
#[inline]
pub fn map_snd<A, B, D>(f: impl Fn(B) -> D) -> impl Fn((A, B)) -> (A, D) {
  move |(a, b)| (a, f(b))
}

/// Swap the components of a pair.
///
/// This witnesses the commutativity of products: `(A, B) ≅ (B, A)`.
///
/// # Laws
///
/// - **Involution**: `swap(swap(p)) = p`
///
/// # Examples
///
/// ```rust
/// use id_effect::foundation::product::swap;
///
/// assert_eq!(swap((1, "hello")), ("hello", 1));
/// assert_eq!(swap(swap((1, 2))), (1, 2));
/// ```
#[inline]
pub fn swap<A, B>(pair: (A, B)) -> (B, A) {
  (pair.1, pair.0)
}

/// Duplicate a value into a pair.
///
/// `dup(a) = (a, a)`
///
/// This is the diagonal morphism: `Δ: A → A × A`.
///
/// # Examples
///
/// ```rust
/// use id_effect::foundation::product::dup;
///
/// assert_eq!(dup(5), (5, 5));
/// ```
#[inline]
pub fn dup<A: Clone>(a: A) -> (A, A) {
  (a.clone(), a)
}

/// Associate a nested pair to the left.
///
/// `assoc_l(((a, b), c)) = (a, (b, c))`
///
/// # Examples
///
/// ```rust
/// use id_effect::foundation::product::assoc_l;
///
/// assert_eq!(assoc_l(((1, 2), 3)), (1, (2, 3)));
/// ```
#[inline]
pub fn assoc_l<A, B, C>(pair: ((A, B), C)) -> (A, (B, C)) {
  let ((a, b), c) = pair;
  (a, (b, c))
}

/// Associate a nested pair to the right.
///
/// `assoc_r((a, (b, c))) = ((a, b), c)`
///
/// # Examples
///
/// ```rust
/// use id_effect::foundation::product::assoc_r;
///
/// assert_eq!(assoc_r((1, (2, 3))), ((1, 2), 3));
/// ```
#[inline]
pub fn assoc_r<A, B, C>(pair: (A, (B, C))) -> ((A, B), C) {
  let (a, (b, c)) = pair;
  ((a, b), c)
}

#[cfg(test)]
mod tests {
  use super::*;
  use rstest::rstest;

  mod fst_tests {
    use super::*;

    #[rstest]
    #[case::int_string((42_i32, "hello"), 42)]
    #[case::int_int((1_i32, 2_i32), 1)]
    #[case::negative((-5_i32, 10_i32), -5)]
    fn fst_extracts_first_component(#[case] pair: (i32, impl Sized), #[case] expected: i32) {
      assert_eq!(fst(pair), expected);
    }

    #[test]
    fn fst_with_complex_types() {
      let pair = (vec![1, 2, 3], "hello");
      assert_eq!(fst(pair), vec![1, 2, 3]);
    }
  }

  mod snd_tests {
    use super::*;

    #[rstest]
    #[case::int_string((42_i32, "hello"), "hello")]
    #[case::int_int((1_i32, 2_i32), 2_i32)]
    fn snd_extracts_second_component<B: PartialEq + std::fmt::Debug>(
      #[case] pair: (i32, B),
      #[case] expected: B,
    ) {
      assert_eq!(snd(pair), expected);
    }

    #[test]
    fn snd_with_complex_types() {
      let pair = ("hello", vec![1, 2, 3]);
      assert_eq!(snd(pair), vec![1, 2, 3]);
    }
  }

  mod pair_tests {
    use super::*;

    #[test]
    fn pair_applies_both_functions() {
      let double = |n: i32| n * 2;
      let negate = |n: i32| -n;
      let both = pair(double, negate);

      assert_eq!(both(5), (10, -5));
    }

    #[test]
    fn pair_with_identity() {
      let id = |n: i32| n;
      let both = pair(id, id);
      assert_eq!(both(7), (7, 7));
    }

    #[rstest]
    #[case::positive(5_i32, (10_i32, -5_i32))]
    #[case::zero(0_i32, (0_i32, 0_i32))]
    #[case::negative(-3_i32, (-6_i32, 3_i32))]
    fn pair_double_and_negate(#[case] input: i32, #[case] expected: (i32, i32)) {
      let both = pair(|n: i32| n * 2, |n: i32| -n);
      assert_eq!(both(input), expected);
    }
  }

  mod bimap_product_tests {
    use super::*;

    #[test]
    fn bimap_transforms_both_components() {
      let double = |n: i32| n * 2;
      let len = |s: &str| s.len();
      let transform = bimap_product(double, len);

      assert_eq!(transform((5, "hello")), (10, 5));
    }

    #[test]
    fn bimap_with_identity_is_identity() {
      let transform = bimap_product(|x: i32| x, |y: i32| y);
      assert_eq!(transform((3, 4)), (3, 4));
    }

    #[test]
    fn bimap_composes() {
      let f1 = bimap_product(|n: i32| n + 1, |n: i32| n * 2);
      let f2 = bimap_product(|n: i32| n * 10, |n: i32| n - 1);

      // bimap(f1, g1) ∘ bimap(f2, g2) = bimap(f1 ∘ f2, g1 ∘ g2)
      let composed = |p: (i32, i32)| f2(f1(p));
      let direct = bimap_product(|n: i32| (n + 1) * 10, |n: i32| n * 2 - 1);

      assert_eq!(composed((5, 3)), direct((5, 3)));
    }
  }

  mod map_fst_tests {
    use super::*;

    #[test]
    fn map_fst_transforms_first_only() {
      let transform = map_fst(|n: i32| n * 2);
      assert_eq!(transform((5, "hello")), (10, "hello"));
    }

    #[test]
    fn map_fst_preserves_second() {
      let transform = map_fst(|_: i32| 999);
      assert_eq!(transform((5, "unchanged")), (999, "unchanged"));
    }
  }

  mod map_snd_tests {
    use super::*;

    #[test]
    fn map_snd_transforms_second_only() {
      let transform = map_snd(|s: &str| s.len());
      assert_eq!(transform((5, "hello")), (5, 5));
    }

    #[test]
    fn map_snd_preserves_first() {
      let transform = map_snd(|_: &str| "changed");
      assert_eq!(transform((42, "hello")), (42, "changed"));
    }
  }

  mod swap_tests {
    use super::*;

    #[test]
    fn swap_exchanges_components() {
      assert_eq!(swap((1, "hello")), ("hello", 1));
    }

    #[test]
    fn swap_involution_law() {
      let pair = (42, "test");
      assert_eq!(swap(swap(pair)), pair);
    }

    #[rstest]
    #[case::int_int((1_i32, 2_i32), (2_i32, 1_i32))]
    #[case::same_type((5_i32, 5_i32), (5_i32, 5_i32))]
    fn swap_cases(#[case] input: (i32, i32), #[case] expected: (i32, i32)) {
      assert_eq!(swap(input), expected);
    }
  }

  mod dup_tests {
    use super::*;

    #[rstest]
    #[case::integer(5_i32, (5_i32, 5_i32))]
    #[case::zero(0_i32, (0_i32, 0_i32))]
    fn dup_creates_pair(#[case] input: i32, #[case] expected: (i32, i32)) {
      assert_eq!(dup(input), expected);
    }

    #[test]
    fn dup_with_string() {
      let s = String::from("hello");
      let (a, b) = dup(s);
      assert_eq!(a, "hello");
      assert_eq!(b, "hello");
    }
  }

  mod assoc_tests {
    use super::*;

    #[test]
    fn assoc_l_flattens_left() {
      assert_eq!(assoc_l(((1, 2), 3)), (1, (2, 3)));
    }

    #[test]
    fn assoc_r_flattens_right() {
      assert_eq!(assoc_r((1, (2, 3))), ((1, 2), 3));
    }

    #[test]
    fn assoc_l_and_assoc_r_are_inverses() {
      let left_nested = ((1, 2), 3);
      let right_nested = assoc_l(left_nested);
      assert_eq!(assoc_r(right_nested), left_nested);
    }
  }

  mod laws {
    use super::*;

    #[test]
    fn projection_reconstruction() {
      // (fst(p), snd(p)) = p
      let p = (42, "hello");
      let reconstructed = (fst((42, "hello")), snd((42, "hello")));
      assert_eq!(reconstructed, p);
    }

    #[test]
    fn pair_then_fst_equals_first_function() {
      // fst ∘ pair(f, g) = f
      let f = |n: i32| n * 2;
      let g = |n: i32| n + 1;
      let paired = pair(f, g);

      for x in [0, 5, -3, 100] {
        assert_eq!(fst(paired(x)), f(x));
      }
    }

    #[test]
    fn pair_then_snd_equals_second_function() {
      // snd ∘ pair(f, g) = g
      let f = |n: i32| n * 2;
      let g = |n: i32| n + 1;
      let paired = pair(f, g);

      for x in [0, 5, -3, 100] {
        assert_eq!(snd(paired(x)), g(x));
      }
    }

    #[test]
    fn bimap_identity_is_identity() {
      let id_bimap = bimap_product(|x: i32| x, |y: i32| y);
      let p = (5, 10);
      assert_eq!(id_bimap(p), p);
    }

    #[test]
    fn swap_commutes_with_bimap() {
      // swap ∘ bimap(f, g) = bimap(g, f) ∘ swap
      let f = |n: i32| n * 2;
      let g = |n: i32| n + 1;

      let left_side = |p: (i32, i32)| swap(bimap_product(f, g)(p));
      let right_side = |p: (i32, i32)| bimap_product(g, f)(swap(p));

      for p in [(1, 2), (0, 0), (-5, 10)] {
        assert_eq!(left_side(p), right_side(p));
      }
    }
  }
}
