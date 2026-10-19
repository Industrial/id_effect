// H8: public routes, guarded routes and decided plain endpoints are accepted.
use std::convert::Infallible;

use axum::http::Method;
use axum::http::request::Parts;
use id_effect::{Effect, succeed};
use id_effect_axum::route::{Endpoint, GuardFuture, Input, Route, RouteGuard, RouteInfo, Router};

struct Open;

impl RouteGuard for Open {
  fn policy(&self) -> &str {
    "open"
  }

  fn check<'a>(&'a self, _: &'a Parts) -> GuardFuture<'a> {
    Box::pin(async { Ok(()) })
  }
}

fn hello(_: &mut (), _: Input) -> Effect<&'static str, Infallible, ()> {
  succeed("hello")
}

fn main() {
  let plain = Endpoint::from_parts(
    RouteInfo::new(Method::GET, "/plain"),
    axum::routing::get(|| async { "plain" }),
  );
  let _: axum::Router = Router::new()
    .route(Route::get("/public").public().handler(hello))
    .route(Route::get("/guarded").guard(Open).handler(hello))
    .route(Route::post("/guarded").handler(hello).guard(Open))
    .route(plain.public())
    .into_axum();
}
