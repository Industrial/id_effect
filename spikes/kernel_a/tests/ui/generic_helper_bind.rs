// (A-specific, risk R3(a)/(d)) Binding a helper that is generic in `R` with `~`: `~` narrows from
// any sub-environment, so nothing fixes the helper's `R`. Corpus H17 writes
// `~audit::<SignupCaps, _>(..)` instead.
use kernel_a::{Effect, HList, Selector, caps, effect};
use spike_corpus::{AppError, Db, Logger};

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
