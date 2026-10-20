// H8: a typed route must say who may call it before `Router::route` accepts it.
use std::convert::Infallible;

use id_effect::{Effect, succeed};
use id_effect_axum::route::{Input, Route, Router};

fn hello(_: &mut (), _: Input) -> Effect<&'static str, Infallible, ()> {
  succeed("hello")
}

fn main() {
  let _ = Router::new().route(Route::get("/hello").handler(hello));
}
