// Gallery: `.provide_some(hlist![..])` with a capability the effect does not need.
// Fix: drop `Cache` from the list.
use id_effect::Never;
use id_effect::{Effect, caps, effect, hlist};

pub struct Db;

pub struct Logger;

pub struct Cache;

fn job() -> Effect<(), Never, caps!(Db, Logger)> {
  effect!({
    let _db = ~Db;
    let _log = ~Logger;
  })
}

fn main() {
  let _ = job().provide_some(hlist![Db, Cache]);
}
