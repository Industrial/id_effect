//! Runtime value matcher — mirrors Effect.ts-style `Match` / pattern routing.
//!
//! Build a [`Matcher`] with ordered [`Matcher::when`] and [`Matcher::tag`] arms, optional
//! [`Matcher::or_else`], then finalize as [`Matcher::exhaustive`], [`Matcher::option`], or
//! [`Matcher::either`].

use std::sync::Arc;

use crate::foundation::predicate::Predicate;

// ── HasTag ────────────────────────────────────────────────────────────────────

/// Values that expose a string tag for [`Matcher::tag`] routing.
pub trait HasTag {
  /// Discriminant string compared against the tag passed to [`Matcher::tag`].
  fn tag(&self) -> &str;
}

// ── Matcher ─────────────────────────────────────────────────────────────────

enum Arm<I, A> {
  When(Predicate<I>, Box<dyn Fn(I) -> A + Send + Sync>),
  Tag(
    Box<dyn Fn(&I) -> bool + Send + Sync>,
    Box<dyn Fn(I) -> A + Send + Sync>,
  ),
}

/// Ordered pattern matcher: first matching arm wins.
pub struct Matcher<I, A> {
  arms: Vec<Arm<I, A>>,
  default: Option<Box<dyn Fn(I) -> A + Send + Sync>>,
}

impl<I: 'static, A: 'static> Matcher<I, A> {
  /// Empty matcher with no arms and no default.
  pub fn new() -> Self {
    Self {
      arms: Vec::new(),
      default: None,
    }
  }

  /// Add a predicate arm evaluated before later arms.
  pub fn when(
    mut self,
    pred: Predicate<I>,
    handler: impl Fn(I) -> A + Send + Sync + 'static,
  ) -> Self {
    self.arms.push(Arm::When(pred, Box::new(handler)));
    self
  }

  /// Add a tag arm: matches when [`HasTag::tag`] equals `expected` (string equality).
  pub fn tag(
    mut self,
    expected: impl Into<String>,
    handler: impl Fn(I) -> A + Send + Sync + 'static,
  ) -> Self
  where
    I: HasTag,
  {
    let expected = expected.into();
    let pred: Box<dyn Fn(&I) -> bool + Send + Sync> =
      Box::new(move |i: &I| i.tag() == expected.as_str());
    self.arms.push(Arm::Tag(pred, Box::new(handler)));
    self
  }

  /// Fallback when no arm matches (used by [`Matcher::exhaustive`] only).
  pub fn or_else(mut self, handler: impl Fn(I) -> A + Send + Sync + 'static) -> Self {
    self.default = Some(Box::new(handler));
    self
  }

  fn try_match(&self, input: I) -> Result<A, I> {
    for arm in &self.arms {
      match arm {
        Arm::When(pred, h) => {
          if pred(&input) {
            return Ok(h(input));
          }
        }
        Arm::Tag(pred, h) => {
          if pred(&input) {
            return Ok(h(input));
          }
        }
      }
    }
    Err(input)
  }

  /// Run this matcher once. Unlike [`Self::exhaustive`], does not require `I: Send` (no shareable closure).
  pub fn run_exhaustive(self, input: I) -> A {
    let input = input;
    for arm in self.arms {
      match arm {
        Arm::When(pred, h) => {
          if pred(&input) {
            return h(input);
          }
        }
        Arm::Tag(pred, h) => {
          if pred(&input) {
            return h(input);
          }
        }
      }
    }
    if let Some(d) = self.default {
      d(input)
    } else {
      panic!("matcher: non-exhaustive match (no arm matched and no or_else)");
    }
  }

  /// `Fn(I) -> A` that panics when nothing matches and no [`Matcher::or_else`] was set.
  ///
  /// The closure is only `Send + Sync` when `I: Send` (it captures `Arc<Matcher<…>>` and may be
  /// invoked with `I` across threads).
  pub fn exhaustive(self) -> impl Fn(I) -> A
  where
    I: 'static,
    A: 'static,
  {
    let shared = Arc::new(self);
    move |input: I| {
      let m = &*shared;
      match m.try_match(input) {
        Ok(a) => a,
        Err(inp) => {
          if let Some(d) = &m.default {
            d(inp)
          } else {
            panic!("matcher: non-exhaustive match (no arm matched and no or_else)");
          }
        }
      }
    }
  }

  /// `Fn(I) -> Option<A>` — `None` when unmatched; **`or_else` is ignored**.
  pub fn option(self) -> impl Fn(I) -> Option<A> + Send + Sync
  where
    I: 'static,
    A: 'static,
  {
    let shared = Arc::new(self);
    move |input: I| shared.try_match(input).ok()
  }

  /// `Fn(I) -> Result<A, I>` — `Err` carries the original input when unmatched; **`or_else` is ignored**.
  pub fn either(self) -> impl Fn(I) -> Result<A, I> + Send + Sync
  where
    I: 'static,
    A: 'static,
  {
    let shared = Arc::new(self);
    move |input: I| shared.try_match(input)
  }
}

impl<I: 'static, A: 'static> Default for Matcher<I, A> {
  fn default() -> Self {
    Self::new()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::foundation::predicate::predicate;

  #[derive(Debug, Clone, Copy)]
  struct Tagged(&'static str, i32);

  impl HasTag for Tagged {
    fn tag(&self) -> &str {
      self.0
    }
  }

  // ── when ──────────────────────────────────────────────────────────────────

  mod when_arms {
    use super::*;

    #[test]
    fn matcher_when_routes_to_first_match() {
      let m = Matcher::new()
        .when(predicate::is_zero_i64(), |n| format!("zero:{n}"))
        .when(Box::new(|n: &i64| *n > 0), |n| format!("pos:{n}"));
      let f = m.exhaustive();
      assert_eq!(f(0), "zero:0");
      assert_eq!(f(3), "pos:3");
    }

    #[rstest::rstest]
    #[case(0_i64, "zero:0")]
    #[case(-1_i64, "neg:-1")]
    #[case(5_i64, "other:5")]
    fn matcher_when_parameterized_routes(#[case] n: i64, #[case] expected: &'static str) {
      let m = Matcher::new()
        .when(predicate::is_zero_i64(), |v| format!("zero:{v}"))
        .when(Box::new(|x: &i64| *x < 0), |v| format!("neg:{v}"))
        .or_else(|v| format!("other:{v}"));
      let f = m.exhaustive();
      assert_eq!(f(n), expected);
    }
  }

  // ── tag ─────────────────────────────────────────────────────────────────────

  mod tag_arms {
    use super::*;

    #[test]
    fn matcher_tag_dispatches_by_tag_field() {
      let m = Matcher::new()
        .tag("a", |t: Tagged| t.1 * 10)
        .tag("b", |t: Tagged| t.1 + 1)
        .or_else(|t: Tagged| t.1);
      let f = m.exhaustive();
      assert_eq!(f(Tagged("a", 2)), 20);
      assert_eq!(f(Tagged("b", 2)), 3);
      assert_eq!(f(Tagged("c", 7)), 7);
    }
  }

  // ── exhaustive ──────────────────────────────────────────────────────────────

  mod exhaustive_finalize {
    use super::*;

    #[test]
    #[should_panic(expected = "non-exhaustive match")]
    fn matcher_exhaustive_panics_when_no_match() {
      let m = Matcher::new().when(predicate::is_zero_i64(), |n: i64| n);
      let f = m.exhaustive();
      let _ = f(1_i64);
    }

    #[test]
    fn matcher_exhaustive_uses_or_else_on_miss() {
      let m = Matcher::new()
        .when(predicate::is_zero_i64(), |n| n)
        .or_else(|n| n + 100);
      let f = m.exhaustive();
      assert_eq!(f(0), 0);
      assert_eq!(f(5), 105);
    }
  }

  // ── option ──────────────────────────────────────────────────────────────────

  mod option_finalize {
    use super::*;

    #[test]
    fn matcher_option_returns_none_for_unmatched() {
      let m = Matcher::new()
        .when(predicate::is_zero_i64(), |n| n)
        .or_else(|n| n + 1);
      let f = m.option();
      assert_eq!(f(0), Some(0));
      assert_eq!(f(3), None);
    }
  }

  // ── either ───────────────────────────────────────────────────────────────────

  mod either_finalize {
    use super::*;

    #[test]
    fn matcher_either_returns_err_original() {
      let m = Matcher::new()
        .when(predicate::is_zero_i64(), |n| n.to_string())
        .or_else(|n| n.to_string());
      let f = m.either();
      assert_eq!(f(0), Ok("0".into()));
      assert_eq!(f(7), Err(7));
    }
  }
}
