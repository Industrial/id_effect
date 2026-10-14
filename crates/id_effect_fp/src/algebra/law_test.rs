//! Law-checking helpers and macros for algebraic structures.
//!
//! Use in unit tests and property tests to verify monad and applicative laws
//! for concrete type constructors such as [`Option`] and [`Result`].
//!
//! The [`law_test!`] macro expects `f` and `g` to be **`fn` items** (not closure
//! literals) so Rust can reuse one function pointer type across law checks.

/// Generate monad law checks for a type constructor.
#[macro_export]
macro_rules! law_test {
  (
    monad $name:ident {
      pure = $pure:expr,
      flat_map = $flat_map:expr,
      fa = $fa:expr,
      a = $a:expr,
      f = $f:expr,
      g = $g:expr $(,)?
    }
  ) => {
    #[test]
    fn $name() {
      let pure = $pure;
      let flat_map = $flat_map;
      assert_eq!(
        flat_map(pure($a), $f),
        $f($a),
        "{}: left identity failed",
        stringify!($name)
      );
      assert_eq!(
        flat_map($fa, pure),
        $fa,
        "{}: right identity failed",
        stringify!($name)
      );
      let lhs = flat_map(flat_map($fa, $f), $g);
      let rhs = flat_map($fa, |x| flat_map($f(x), $g));
      assert_eq!(lhs, rhs, "{}: associativity failed", stringify!($name));
    }
  };
}

#[cfg(test)]
mod tests {
  use crate::algebra::monad::{option, result};

  fn inc(x: i32) -> Option<i32> {
    Some(x + 1)
  }
  fn double(x: i32) -> Option<i32> {
    Some(x * 2)
  }

  #[test]
  fn option_monad_laws() {
    let fa = Some(3);
    assert_eq!(option::flat_map(option::pure(7), inc), inc(7));
    assert_eq!(option::flat_map(fa, option::pure), fa);
    assert_eq!(
      option::flat_map(option::flat_map(fa, inc), double),
      option::flat_map(fa, |x| option::flat_map(inc(x), double))
    );
  }

  fn inc_result(x: i32) -> Result<i32, &'static str> {
    Ok(x + 1)
  }
  fn double_result(x: i32) -> Result<i32, &'static str> {
    Ok(x * 2)
  }

  #[test]
  fn result_monad_laws() {
    let fa: Result<i32, &str> = Ok(3);
    assert_eq!(
      result::flat_map(result::pure::<_, &str>(7), inc_result),
      inc_result(7)
    );
    assert_eq!(result::flat_map(fa, result::pure::<_, &str>), fa);
    assert_eq!(
      result::flat_map(result::flat_map(fa, inc_result), double_result),
      result::flat_map(fa, |x| result::flat_map(inc_result(x), double_result))
    );
  }

  #[test]
  fn applicative_identity_option() {
    use crate::algebra::applicative::option;
    let fa = Some(42);
    let id = |x: i32| x;
    assert_eq!(option::ap(Some(id), fa.clone()), fa);
  }

  #[test]
  fn applicative_homomorphism_option() {
    use crate::algebra::applicative::option;
    let f = |x: i32| x * 2;
    assert_eq!(
      option::ap(option::pure(f), option::pure(5)),
      option::pure(f(5))
    );
  }
}
