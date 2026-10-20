// ADR 0013 R1 (H8): a typed route must say who may call it before `Router::route` accepts it.
use std::convert::Infallible;

use id_effect_kit::axum::route::{Input, Route, Router};
use id_effect_kit::id_effect::{Effect, succeed};

fn hello(_: &mut (), _: Input) -> Effect<&'static str, Infallible, ()> {
  succeed("hello")
}

fn main() {
  let _ = Router::new().route(Route::get("/hello").handler(hello));
}
