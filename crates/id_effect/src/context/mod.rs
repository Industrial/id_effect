//! **Stratum 3 — Environment & context** ([`Tag`], [`Tagged`], [`Cons`] / [`Nil`], paths, [`Get`] / [`GetMut`], [`Context`]).
//!
//! Built only from lower strata (phantom data, products, type-level wiring). Lookup is **compile-time**;
//! missing tags or wrong paths are **type errors**.
//!
//! See `SPEC.md` §3 and `TESTING.md` for test layout.
//!
//! ## Additional modules
//!
//! | Submodule | Provides | Depends on |
//! |-----------|----------|------------|
//! | [`match_`] | [`Matcher`], [`HasTag`] | [`foundation::predicate`] (Stratum 0) |

mod get;
mod hlist;
mod path;
mod tag;
mod tagged;
mod wrapper;

pub mod match_;
pub mod optics;

pub use get::{Get, GetMut};
pub use hlist::{Cons, Nil};
pub use match_::{HasTag, Matcher};
pub use optics::{EnvLens, focus, identity_lens};
pub use path::{Here, Skip0, Skip1, Skip2, Skip3, Skip4, There, ThereHere};
pub use tag::Tag;
pub use tagged::{Tagged, tagged};
pub use wrapper::{Context, prepend_cell};

#[cfg(test)]
mod tests {
  use super::*;
  use rstest::rstest;

  #[derive(Debug)]
  struct DbKey;
  #[derive(Debug)]
  struct ClockKey;
  #[derive(Debug)]
  struct ExtraKey;

  type Ctx = Context<Cons<Tagged<DbKey, i32>, Cons<Tagged<ClockKey, u64>, Nil>>>;

  type CtxThree = Context<
    Cons<
      Tagged<DbKey, i32>,
      Cons<Tagged<ClockKey, u64>, Cons<Tagged<ExtraKey, &'static str>, Nil>>,
    >,
  >;

  // ========== Fixtures ==========

  fn sample_context() -> Ctx {
    Context::new(Cons(
      Tagged::<DbKey, _>::new(42),
      Cons(Tagged::<ClockKey, _>::new(7u64), Nil),
    ))
  }

  fn sample_context_three() -> CtxThree {
    Context::new(Cons(
      Tagged::<DbKey, _>::new(1),
      Cons(
        Tagged::<ClockKey, _>::new(2u64),
        Cons(Tagged::<ExtraKey, _>::new("tail"), Nil),
      ),
    ))
  }

  mod tag {
    use super::*;
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    #[test]
    fn default_when_constructed_matches_new_for_same_key() {
      assert_eq!(Tag::<DbKey>::new(), Tag::<DbKey>::default());
    }

    #[test]
    fn new_when_constructed_twice_produces_equal_tags_for_same_key() {
      assert_eq!(Tag::<DbKey>::new(), Tag::<DbKey>::new());
    }

    #[test]
    fn debug_when_formatted_renders_tag_marker() {
      assert_eq!(format!("{:?}", Tag::<DbKey>::new()), "Tag");
    }

    #[test]
    fn hash_when_same_key_type_is_used_produces_stable_value() {
      let mut left = DefaultHasher::new();
      let mut right = DefaultHasher::new();
      Tag::<DbKey>::new().hash(&mut left);
      Tag::<DbKey>::default().hash(&mut right);
      assert_eq!(left.finish(), right.finish());
    }
  }

  mod tagged {
    use super::*;

    #[rstest]
    #[case::zero(0)]
    #[case::positive(100)]
    #[case::negative(-7)]
    fn new_with_value_stores_value_under_phantom_key(#[case] value: i32) {
      let t = Tagged::<DbKey, _>::new(value);
      assert_eq!(t.value, value);
    }

    #[rstest]
    #[case::zero(0)]
    #[case::positive(3)]
    fn tagged_free_function_matches_new(#[case] value: i32) {
      assert_eq!(
        tagged::<DbKey, _>(value).value,
        Tagged::<DbKey, _>::new(value).value
      );
    }
  }

  mod hlist {
    use super::*;

    #[test]
    fn cons_when_built_carries_head_and_tail() {
      let c = Cons(Tagged::<DbKey, _>::new(1), Nil);
      assert_eq!(c.0.value, 1);
    }

    #[test]
    fn nil_when_used_is_singleton_type_marker() {
      let _a: Nil = Nil;
      let _b: Nil = Nil;
      assert_eq!(_a, _b);
    }
  }

  mod path_aliases {
    use super::*;

    #[test]
    fn skip0_when_used_matches_here_for_type_identity() {
      let _: Here = Skip0::default();
    }
  }

  mod context_wrapper {
    use super::*;

    #[test]
    fn new_with_inner_list_wraps_list_in_context() {
      let inner = Cons(Tagged::<DbKey, _>::new(0), Nil);
      let ctx = Context::new(inner);
      assert_eq!(ctx.0.0.value, 0);
    }

    #[test]
    fn into_inner_when_called_returns_wrapped_list() {
      let inner = Cons(Tagged::<DbKey, _>::new(5), Nil);
      let ctx = Context::new(inner);
      assert_eq!(ctx.into_inner().0.value, 5);
    }

    #[test]
    fn as_ref_and_as_mut_when_used_allow_roundtrip_mutation_of_inner_list() {
      let inner = Cons(Tagged::<DbKey, _>::new(3), Nil);
      let mut ctx = Context::new(inner);
      assert_eq!(ctx.as_ref().0.value, 3);
      ctx.as_mut().0.value = 8;
      assert_eq!(ctx.as_ref().0.value, 8);
    }
  }

  mod prepend_and_projection {
    use super::*;

    #[test]
    fn prepend_when_given_head_and_context_builds_cons_wrapped_context() {
      let base = Context::new(Cons(Tagged::<ClockKey, _>::new(2u64), Nil));
      let extended = prepend_cell(Tagged::<DbKey, _>::new(1), base);
      assert_eq!(*extended.get::<DbKey>(), 1);
      assert_eq!(*extended.get_path::<ClockKey, Skip1>(), 2);
    }

    #[test]
    fn prepend_method_when_called_matches_prepend_cell() {
      let base = Context::new(Cons(Tagged::<DbKey, _>::new(9), Nil));
      let a = base.prepend(Tagged::<ClockKey, _>::new(8u64));
      let b = prepend_cell(
        Tagged::<ClockKey, _>::new(8u64),
        Context::new(Cons(Tagged::<DbKey, _>::new(9), Nil)),
      );
      assert_eq!(*a.get::<ClockKey>(), *b.get::<ClockKey>());
      assert_eq!(*a.get_path::<DbKey, Skip1>(), *b.get_path::<DbKey, Skip1>());
    }

    #[test]
    fn head_and_tail_list_when_used_split_cons_context() {
      let ctx = sample_context();
      assert_eq!(ctx.head().value, 42);
      assert_eq!(ctx.tail_list().0.value, 7u64);
    }

    #[test]
    fn into_tail_when_called_drops_head_preserving_tail_context() {
      let ctx = sample_context();
      let tail = ctx.into_tail();
      assert_eq!(*tail.get::<ClockKey>(), 7u64);
    }
  }

  mod get_at_here {
    use super::*;

    #[test]
    fn get_with_head_path_returns_value_registered_in_head_cell() {
      let ctx = sample_context();
      assert_eq!(*ctx.get::<DbKey>(), 42);
    }

    #[test]
    fn get_trait_impl_for_context_returns_head_value() {
      let ctx = sample_context();
      assert_eq!(*Get::<DbKey, Here>::get(&ctx), 42);
    }

    #[test]
    fn get_path_with_skip0_matches_get_at_here() {
      let ctx = sample_context();
      assert_eq!(*ctx.get_path::<DbKey, Skip0>(), *ctx.get::<DbKey>());
    }
  }

  mod get_path {
    use super::*;

    #[test]
    fn get_path_with_skip1_finds_second_tagged_cell() {
      let ctx = sample_context();
      assert_eq!(*ctx.get_path::<ClockKey, Skip1>(), 7);
    }

    #[test]
    fn get_path_with_skip2_finds_third_tagged_cell() {
      let ctx = sample_context_three();
      assert_eq!(*ctx.get_path::<ExtraKey, Skip2>(), "tail");
    }
  }

  mod get_mut_at_here {
    use super::*;

    #[test]
    fn get_mut_with_head_path_mutates_head_value() {
      let mut ctx = Context::new(Cons(
        Tagged::<DbKey, _>::new(1),
        Cons(Tagged::<ClockKey, _>::new(2u64), Nil),
      ));
      *ctx.get_mut::<DbKey>() = 99;
      assert_eq!(*ctx.get::<DbKey>(), 99);
    }

    #[test]
    fn get_mut_trait_impl_for_context_mutates_head_value() {
      let mut ctx = sample_context();
      *GetMut::<DbKey, Here>::get_mut(&mut ctx) = 55;
      assert_eq!(*ctx.get::<DbKey>(), 55);
    }
  }

  mod get_mut_path {
    use super::*;

    #[test]
    fn get_mut_path_with_skip1_mutates_second_cell_without_touching_head() {
      let mut ctx = sample_context();
      *ctx.get_mut_path::<ClockKey, Skip1>() = 100u64;
      assert_eq!(*ctx.get::<DbKey>(), 42);
      assert_eq!(*ctx.get_path::<ClockKey, Skip1>(), 100);
    }

    #[test]
    fn get_mut_path_with_skip2_mutates_third_cell() {
      let mut ctx = sample_context_three();
      *ctx.get_mut_path::<ExtraKey, Skip2>() = "updated";
      assert_eq!(*ctx.get_path::<ExtraKey, Skip2>(), "updated");
      assert_eq!(*ctx.get::<DbKey>(), 1);
    }
  }
}
