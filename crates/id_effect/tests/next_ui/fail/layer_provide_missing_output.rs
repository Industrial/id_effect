// (5) `provide_layer` with a layer whose output lacks a capability the effect needs: rejected in
// capability terms (`Embed`), never as E0283. Duplicate *outputs* are rejected when the layer
// graph is wired (a runtime check, the `k-layer-base` route (b) fallback), so they have no UI case.
use id_effect::next::{Effect, Layer, caps_next, effect_next, hlist};

pub struct AppError;

pub struct Db;

pub struct Mailer;

fn handler() -> Effect<(), AppError, caps_next!(Db, Mailer)> {
  effect_next!({
    let _db = ~Db;
    let _mailer = ~Mailer;
  })
}

fn db_layer() -> Layer<caps_next!(Db), AppError, caps_next!()> {
  Layer::succeed(hlist![Db])
}

fn main() {
  let _ = handler().provide_layer(db_layer());
}
