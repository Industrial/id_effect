// ADR 0013 R1 (H8, L9 shim): `route_unchecked` still routes an undecided endpoint, but it is
// deprecated, so `deny(deprecated)` builds reject it.
#![deny(deprecated)]

use std::convert::Infallible;

use id_effect_kit::axum::route::{Input, Route, Router};
use id_effect_kit::id_effect::{Effect, succeed};

fn hello(_: &mut (), _: Input) -> Effect<&'static str, Infallible, ()> {
  succeed("hello")
}

fn main() {
  let _ = Router::new().route_unchecked(Route::get("/hello").handler(hello));
}
