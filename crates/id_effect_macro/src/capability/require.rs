//! `require!` macro.

/// Borrow a capability from the environment parameter.
#[macro_export]
macro_rules! require {
  ($env:expr, $key:ty) => {
    ::id_effect::Needs::<$key>::need($env)
  };
}
