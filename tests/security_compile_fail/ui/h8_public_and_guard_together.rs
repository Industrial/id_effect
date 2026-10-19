// ADR 0013 R1 (H8): a guarded route cannot be declared public afterwards, which would advertise
// it as open to anonymous callers; `.public()` exists only on an undecided route.
use std::convert::Infallible;

use axum::http::request::Parts;
use id_effect_kit::axum::route::{GuardFuture, Input, Route, RouteGuard, Router};
use id_effect_kit::id_effect::{Effect, succeed};

struct Admin;

impl RouteGuard for Admin {
  fn policy(&self) -> &str {
    "admin"
  }

  fn check<'a>(&'a self, _: &'a Parts) -> GuardFuture<'a> {
    Box::pin(async { Ok(()) })
  }
}

fn hello(_: &mut (), _: Input) -> Effect<&'static str, Infallible, ()> {
  succeed("hello")
}

fn main() {
  let _ = Router::new().route(Route::get("/admin").guard(Admin).public().handler(hello));
}
