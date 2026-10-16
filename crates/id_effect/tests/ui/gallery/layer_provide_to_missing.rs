// Gallery: `base.provide_to(upper)` where `base` does not build all of `upper`'s input.
// Fix: give `base` the missing capability, e.g. `db_layer().merge(config_layer())`.
use id_effect::{Layer, caps, hlist};

pub struct AppError;

pub struct Db;

pub struct Config;

pub struct Repo;

fn db_layer() -> Layer<caps!(Db), AppError> {
  Layer::succeed(hlist![Db])
}

fn repo_layer() -> Layer<caps!(Repo), AppError, caps!(Db, Config)> {
  Layer::effect(|| id_effect::succeed(hlist![Repo]))
}

fn main() {
  let _ = db_layer().provide_to(repo_layer());
}
