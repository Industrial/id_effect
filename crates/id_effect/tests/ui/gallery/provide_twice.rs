// Gallery: providing the same capability twice; the first `.provide(..)` already removed it
// from `R`. Fix: drop the second `.provide(Db)`.
use id_effect::Never;
use id_effect::{Effect, caps, effect};

pub struct Db;

pub struct Logger;

pub struct Clock;

fn job() -> Effect<(), Never, caps!(Db, Logger, Clock)> {
  effect!({
    let _db = ~Db;
    let _log = ~Logger;
    let _now = ~Clock;
  })
}

fn main() {
  let _ = job().provide(Db).provide(Db);
}
