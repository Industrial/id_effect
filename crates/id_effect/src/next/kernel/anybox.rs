//! Type erasure for the runloop: `AnyBox` erase/unbox, `AnyExit`, `AnyEnv`. Filled by
//! `k-core-runloop`, optimised by `k-runloop-opt`.
//!
//! Every value that crosses an erased runloop step travels as an [`AnyBox`]. `k-runloop-opt`
//! owns making this cheaper (statically known types, no per-value `TypeId` ladder); keep every
//! erase/unbox call going through [`erase`] / [`unbox`] / [`erase_exit`] / [`unerase_exit`] so
//! that leaf has a single place to change.

use std::any::Any;
use std::sync::{Arc, LazyLock};

use super::cause::map_fail_mut;
use crate::failure::Cause;
use crate::next::capability::HNil;

/// An erased runloop value (design A §5 R2 mitigation, "SmallBox", in safe Rust): `()` and a
/// fixed set of word-sized primitives (recognised by `TypeId`) travel inline; every other type,
/// including small user structs, is boxed.
pub(crate) enum AnyBox {
  Unit,
  Word(u64),
  Boxed(Box<dyn Any + Send>),
}

/// Erases `t` (inline for `()` and word-sized primitives, boxed otherwise).
pub(crate) fn erase<T: Send + 'static>(t: T) -> AnyBox {
  let mut slot = Some(t);
  let any: &mut dyn Any = &mut slot;
  if any.is::<Option<()>>() {
    return AnyBox::Unit;
  }
  macro_rules! word {
    ($($ty:ty),*) => {$(
      if let Some(s) = any.downcast_mut::<Option<$ty>>() {
        return AnyBox::Word(s.take().map_or(0, |v| v as u64));
      }
    )*};
  }
  word!(u64, usize, u32, i64, i32, bool);
  AnyBox::Boxed(Box::new(
    slot
      .take()
      .expect("next: erase slot is filled (kernel bug)"),
  ))
}

/// Recovers a value erased by [`erase`]. A type mismatch is a kernel bug and panics.
pub(crate) fn unbox<T: 'static>(v: AnyBox) -> T {
  let mut out: Option<T> = None;
  let any: &mut dyn Any = &mut out;
  match v {
    AnyBox::Boxed(b) => match b.downcast::<T>() {
      Ok(t) => return *t,
      Err(_) => panic!("next: erased value type mismatch (kernel bug)"),
    },
    AnyBox::Unit => {
      if let Some(o) = any.downcast_mut::<Option<()>>() {
        *o = Some(());
      }
    }
    AnyBox::Word(w) => {
      macro_rules! word {
        ($($ty:ty),*) => {$(
          if let Some(o) = any.downcast_mut::<Option<$ty>>() {
            *o = Some(w as $ty);
          }
        )*};
      }
      word!(u64, usize, u32, i64, i32);
      if let Some(o) = any.downcast_mut::<Option<bool>>() {
        *o = Some(w != 0);
      }
    }
  }
  out.expect("next: erased value type mismatch (kernel bug)")
}

/// The erased outcome of one runloop step.
pub(crate) type AnyExit = Result<AnyBox, Cause<AnyBox>>;

/// The erased environment of a runloop (an `Arc` of the effect's `R` HList).
pub(crate) type AnyEnv = Arc<dyn Any + Send + Sync>;

/// The shared empty environment (`HNil`) of every run edge: cloning it is a reference-count
/// increment instead of an allocation per run.
pub(crate) fn hnil_env() -> &'static AnyEnv {
  static HNIL_ENV: LazyLock<AnyEnv> = LazyLock::new(|| Arc::new(HNil));
  &HNIL_ENV
}

/// Erases a typed step outcome.
pub(crate) fn erase_exit<A: Send + 'static, E: Send + 'static>(r: Result<A, Cause<E>>) -> AnyExit {
  match r {
    Ok(a) => Ok(erase(a)),
    Err(c) => Err(map_fail_mut(c, &mut erase)),
  }
}

/// Recovers a typed step outcome erased by [`erase_exit`].
pub(crate) fn unerase_exit<A: 'static, E: 'static>(r: AnyExit) -> Result<A, Cause<E>> {
  match r {
    Ok(a) => Ok(unbox(a)),
    Err(c) => Err(map_fail_mut(c, &mut unbox)),
  }
}

/// Borrows the erased environment as `R`. A mismatch is a kernel bug and panics.
pub(crate) fn downcast_env<R: 'static>(env: &AnyEnv) -> &R {
  match (**env).downcast_ref::<R>() {
    Some(r) => r,
    None => panic!("next: environment type mismatch (kernel bug)"),
  }
}

/// Clones the erased environment as `Arc<R>`. A mismatch is a kernel bug and panics.
pub(crate) fn downcast_env_arc<R: Send + Sync + 'static>(env: &AnyEnv) -> Arc<R> {
  match Arc::clone(env).downcast::<R>() {
    Ok(r) => r,
    Err(_) => panic!("next: environment type mismatch (kernel bug)"),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn erase_round_trips_inline_and_boxed_values() {
    assert!(matches!(erase(()), AnyBox::Unit));
    assert!(matches!(erase(7_u32), AnyBox::Word(7)));
    assert_eq!(unbox::<i64>(erase(-3_i64)), -3);
    assert!(unbox::<bool>(erase(true)));
    assert_eq!(unbox::<usize>(erase(usize::MAX)), usize::MAX);
    assert_eq!(unbox::<String>(erase(String::from("x"))), "x");
    assert_eq!(unbox::<(u8, u8)>(erase((1_u8, 2_u8))), (1, 2));
  }

  #[test]
  fn erase_exit_round_trips_causes() {
    let e: Result<u8, Cause<String>> = Err(Cause::both(
      Cause::fail("a".to_owned()),
      Cause::then(Cause::die("d"), Cause::fail("b".to_owned())),
    ));
    let back = unerase_exit::<u8, String>(erase_exit(e.clone()));
    assert_eq!(back, e);
  }
}
