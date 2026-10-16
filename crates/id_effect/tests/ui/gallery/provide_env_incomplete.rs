// Gallery: `.provide_env(hlist![..])` with an environment that lacks a needed capability.
// Fix: put every capability of the effect's `R` in the environment, `hlist![Db, Logger]`.
use id_effect::Never;
use id_effect::{Effect, caps, effect, hlist};

pub struct Db;

pub struct Logger;

fn job() -> Effect<(), Never, caps!(Db, Logger)> {
  effect!({
    let _db = ~Db;
    let _log = ~Logger;
  })
}

fn main() {
  let _ = job().provide_env(hlist![Db]);
}
