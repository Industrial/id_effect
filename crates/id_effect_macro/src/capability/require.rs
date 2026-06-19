//! `require!` macro (use inside `effect!` bodies only).

/// Borrow a capability from the effect environment parameter.
///
/// Inside `effect!`, `~Key` is equivalent sugar (see the `effect!` proc macro).
///
/// Outside `effect!`, use [`Needs`](::id_effect::Needs)::`<Key>::need(env)` directly.
#[macro_export]
macro_rules! require {
  ($key:ty) => {
    compile_error!(
      "require!(Key) is only valid inside effect! bodies; use Needs::<Key>::need(env) elsewhere"
    )
  };
  ($env:expr, $key:ty) => {
    compile_error!(
      "require!(env, Key) was removed in id_effect 3.0; use require!(Key) inside effect! or Needs::<Key>::need(env)"
    )
  };
}
