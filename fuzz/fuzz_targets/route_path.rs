//! Typed-route path decode (id_effect_axum `route::ValidPath`, the decoder typed routes share):
//! the input is the path of a request sent through a router with a struct-schema route
//! (`/items/{id}/{slug}`), a scalar one (`/accounts/{id}`) and a wildcard record one
//! (`/files/{*path}`), so axum's matching and percent-decoding run first, as in a server.
//!
//! Checks besides "no panic": decoding never answers a 5xx.
#![no_main]

use std::sync::LazyLock;

use axum::Router;
use axum::http::Request;
use axum::routing::get;
use id_effect::schema::{self, HasSchema, Schema};
use id_effect_axum::route::ValidPath;
use id_effect_fuzz::{Labels, block_on, uri_text};
use libfuzzer_sys::fuzz_target;
use tower::ServiceExt;

struct Item;

impl HasSchema for Item {
  type A = (i64, String);
  type I = (i64, String);
  type E = ();

  fn schema() -> Schema<Self::A, Self::I> {
    schema::struct_(
      "id",
      schema::minimum(schema::i64(), 0),
      "slug",
      schema::length(schema::string(), 1, 32),
    )
  }
}

struct AccountId;

impl HasSchema for AccountId {
  type A = i64;
  type I = i64;
  type E = ();

  fn schema() -> Schema<i64, i64> {
    schema::range(schema::i64(), 1, i64::MAX)
  }
}

static ROUTER: LazyLock<Router> = LazyLock::new(|| {
  Router::new()
    .route(
      "/items/{id}/{slug}",
      get(|ValidPath((id, slug)): ValidPath<Item>| async move { format!("{id}/{slug}") }),
    )
    .route(
      "/accounts/{id}",
      get(|ValidPath(id): ValidPath<AccountId>| async move { id.to_string() }),
    )
    .route(
      "/files/{*path}",
      get(|ValidPath(labels): ValidPath<Labels>| async move { labels.len().to_string() }),
    )
});

fuzz_target!(|data: &[u8]| {
  let Ok(request) = Request::builder()
    .uri(format!("/{}", uri_text(data)))
    .body(axum::body::Body::empty())
  else {
    return;
  };
  let response = block_on(ROUTER.clone().oneshot(request)).expect("the router is infallible");
  assert!(
    !response.status().is_server_error(),
    "{}",
    response.status()
  );
});
