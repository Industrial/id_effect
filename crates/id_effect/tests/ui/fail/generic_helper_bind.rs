// (H17 residue, risk R3(a)/(d)) Binding a helper that is generic in `R` with a plain `~`: `~`
// narrows from any sub-environment, so nothing fixes the helper's `R`. The pattern is
// `~audit(..).same_env()` (pass/h17_same_env.rs) or a concrete `caps!(..)` helper.
use id_effect::{Effect, HList, Selector, caps, effect};

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
  effect!({ ~Logger.info(msg) })
}

fn signup() -> Effect<(), AppError, caps!(Logger, Db)> {
  effect!({ ~audit("signup".to_string()) })
}

fn main() {
  let _ = signup();
}
