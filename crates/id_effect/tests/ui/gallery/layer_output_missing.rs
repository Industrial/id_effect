// Gallery: `.provide_layer(..)` with a layer that does not build every capability the effect
// needs. Fix: merge in a layer for the missing one, `db_layer().merge(mailer_layer())`.
use id_effect::{Effect, Layer, caps, effect, hlist};

pub struct AppError;

pub struct Db;

pub struct Mailer;

fn signup() -> Effect<(), AppError, caps!(Db, Mailer)> {
  effect!({
    let _db = ~Db;
    let _mailer = ~Mailer;
  })
}

fn db_layer() -> Layer<caps!(Db), AppError, caps!()> {
  Layer::succeed(hlist![Db])
}

fn main() {
  let _ = signup().provide_layer(db_layer());
}
