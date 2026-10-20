// ADR 0013 R1 (H8): a guard is a `RouteGuard` that checks the request, not a policy name.
use std::convert::Infallible;

use id_effect_kit::axum::route::{Input, Route, Router};
use id_effect_kit::id_effect::{Effect, succeed};

fn hello(_: &mut (), _: Input) -> Effect<&'static str, Infallible, ()> {
  succeed("hello")
}

fn main() {
  let _ = Router::new().route(Route::get("/admin").guard("admin").handler(hello));
}
