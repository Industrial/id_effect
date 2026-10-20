// H8 (L9 shim): `route_unchecked` still routes an undecided endpoint, but it is deprecated.
#![deny(deprecated)]

use std::convert::Infallible;

use id_effect::{Effect, succeed};
use id_effect_axum::route::{Input, Route, Router};

fn hello(_: &mut (), _: Input) -> Effect<&'static str, Infallible, ()> {
  succeed("hello")
}

fn main() {
  let _ = Router::new().route_unchecked(Route::get("/hello").handler(hello));
}
