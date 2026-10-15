// (H17 residue, risk R3(a)/(d)) Binding a helper that is generic in `R` with a plain `~`: `~`
// narrows from any sub-environment, so nothing fixes the helper's `R`. The pattern is
// `~audit(..).same_env()` (pass/h17_same_env.rs) or a concrete `caps_next!(..)` helper.
use id_effect::next::{Effect, HList, Selector, caps_next, effect_next};

pub struct AppError;

pub struct Db;

pub struct Logger;

impl Logger {
  pub fn info(&self, _line: String) {}
}

fn audit<R, I>(msg: String) -> Effect<(), AppError, R>
where
  R: HList + Selector<Logger, I>,
{
  effect_next!({ ~Logger.info(msg) })
}

fn signup() -> Effect<(), AppError, caps_next!(Logger, Db)> {
  effect_next!({ ~audit("signup".to_string()) })
}

fn main() {
  let _ = signup();
}
