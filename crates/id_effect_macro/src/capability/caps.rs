//! `caps!` macro.

/// Required capability set for `Effect<_, _, caps!(…)>` (runtime: [`Env`](::id_effect::Env)).
#[macro_export]
macro_rules! caps {
  () => {
    ::id_effect::Env
  };
  ($($cap:ident),+ $(,)?) => {
    ::id_effect::Env
  };
}
