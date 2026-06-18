//! **Bifoldable** — fold over both sides of a sum type.

/// Bifoldable types expose left and right variants.
pub trait Bifoldable {
  /// Left variant type.
  type Left;
  /// Right variant type.
  type Right;
  /// Eliminate both sides with handlers.
  fn bifold<B>(
    self,
    on_left: impl FnOnce(Self::Left) -> B,
    on_right: impl FnOnce(Self::Right) -> B,
  ) -> B;
}

/// [`Either`](crate::foundation::either::Either) helpers.
pub mod either {
  use crate::foundation::either::Either;

  /// Eliminate an `Either`.
  pub fn bifold<L, R, B>(
    e: Either<R, L>,
    on_left: impl FnOnce(L) -> B,
    on_right: impl FnOnce(R) -> B,
  ) -> B {
    match e {
      Err(l) => on_left(l),
      Ok(r) => on_right(r),
    }
  }
}

/// Effectful bitraverse over `Either`.
pub mod bitraverse {
  use crate::foundation::either::Either;
  use crate::kernel::Effect;

  /// Traverse both sides with effects.
  pub fn bitraverse<L, R, L2, R2, E, Rq, FL, FR>(
    e: Either<R, L>,
    fl: FL,
    fr: FR,
  ) -> Effect<Either<R2, L2>, E, Rq>
  where
    FL: FnOnce(L) -> Effect<L2, E, Rq>,
    FR: FnOnce(R) -> Effect<R2, E, Rq>,
  {
    match e {
      Err(l) => fl(l).map(Err),
      Ok(r) => fr(r).map(Ok),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::either::bifold;

  #[test]
  fn bifold_left() {
    assert_eq!(
      bifold::<i32, i32, i32>(Err(1i32), |l: i32| l + 1, |_r: i32| 0),
      2
    );
  }

  #[test]
  fn bifold_right() {
    assert_eq!(
      bifold::<i32, i32, i32>(Ok(1i32), |_l: i32| 0, |r: i32| r + 10),
      11
    );
  }

  mod bitraverse_tests {
    use crate::algebra::bifoldable::bitraverse::bitraverse;
    use crate::foundation::either::Either;
    use crate::{Exit, pure, run_test};

    #[test]
    fn traverses_left_branch() {
      let e: Either<(), i32> = Err(5);
      let exit: Exit<Either<(), String>, ()> = run_test(
        bitraverse(e, |l| pure(l.to_string()), |_r: ()| pure(())),
        (),
      );
      assert_eq!(exit, Exit::succeed(Err("5".into())));
    }

    #[test]
    fn traverses_right_branch() {
      let e: Either<i32, ()> = Ok(7);
      let exit: Exit<Either<String, ()>, ()> = run_test(
        bitraverse(e, |_l: ()| pure(()), |r| pure(r.to_string())),
        (),
      );
      assert_eq!(exit, Exit::succeed(Ok("7".into())));
    }
  }
}
