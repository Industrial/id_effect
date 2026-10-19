// ADR 0013 R1 (H8): a plain Axum endpoint (`Endpoint::from_parts`) is undecided whatever its
// `RouteInfo` says, and `Router::route` refuses it until `.public()` or `.guard(g)`.
use axum::http::Method;
use id_effect_kit::axum::route::{Endpoint, RouteInfo, Router};

fn main() {
  let plain = Endpoint::from_parts(
    RouteInfo::new(Method::GET, "/plain"),
    axum::routing::get(|| async { "plain" }),
  );
  let _ = Router::new().route(plain);
}
