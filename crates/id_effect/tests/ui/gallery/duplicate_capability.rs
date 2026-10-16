// Gallery: the same capability listed twice in `caps!(..)`. Fix: remove the second one.
use id_effect::Never;
use id_effect::{Effect, caps, effect};

pub struct Db;

pub struct Config;

fn handler() -> Effect<(), Never, caps!(Config, Db, Config)> {
  effect!({
    let _c = ~Config;
  })
}

fn main() {
  let _ = handler();
}
