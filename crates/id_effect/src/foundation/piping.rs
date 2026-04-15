//! Left-to-right function application (F#-style `|>`).
//!
//! - [`Pipe::pipe`] — `x.pipe(f).pipe(g)` ≡ `g(f(x))`
//! - [`pipe!`](macro@crate::pipe) — `pipe!(x, f, g, h)` ≡ `h(g(f(x)))` (crate-root macro). Defined in [`mod@crate::macros`].
//!
//! Pairs with [`crate::Effect`] without extra allocation, e.g.
//! `succeed(1).pipe(|e| e.map(|n| n + 1))`.

/// Chain a value through `f` (and further `.pipe` calls on the result).
pub trait Pipe: Sized {
  /// Applies `f` to `self` (F#-style forward pipe).
  #[inline(always)]
  fn pipe<F, R>(self, f: F) -> R
  where
    F: FnOnce(Self) -> R,
  {
    f(self)
  }
}

impl<T: Sized> Pipe for T {}

#[cfg(test)]
mod tests {
  use super::Pipe;
  use rstest::rstest;

  mod pipe_trait {
    use super::*;

    #[rstest]
    #[case::positive(2, 30)]
    #[case::zero(0, 10)]
    #[case::negative(-3, -20)]
    fn pipe_with_chained_functions_applies_functions_left_to_right(
      #[case] input: i32,
      #[case] expected: i32,
    ) {
      let result = input.pipe(|x| x + 1).pipe(|x| x * 10);
      assert_eq!(result, expected);
    }
  }

  mod pipe_macro {
    use super::*;

    #[test]
    fn pipe_macro_with_single_argument_returns_expression_unchanged() {
      assert_eq!(crate::pipe!(2), 2);
    }

    #[rstest]
    #[case::positive(2, 30)]
    #[case::zero(0, 10)]
    #[case::negative(-3, -20)]
    fn pipe_macro_with_two_functions_applies_functions_left_to_right(
      #[case] start: i32,
      #[case] expected: i32,
    ) {
      assert_eq!(
        crate::pipe!(start, |x: i32| x + 1, |x: i32| x * 10),
        expected
      );
    }

    #[rstest]
    #[case::positive(1, 15)]
    #[case::zero(0, 5)]
    fn pipe_macro_with_three_functions_applies_functions_left_to_right(
      #[case] start: i32,
      #[case] expected: i32,
    ) {
      assert_eq!(
        crate::pipe!(start, |x: i32| x + 1, |x: i32| x * 10, |x: i32| x - 5),
        expected
      );
    }
  }
}
