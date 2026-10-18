//! Typed-route query decode (id_effect_axum `route::ValidQuery`, the decoder typed routes share):
//! the input is the query string of a request, decoded into a struct schema (integer, number,
//! repeated string and boolean coercion) or a record schema, chosen by the first byte.
//!
//! Checks besides "no panic": an `Invalid` rejection lists at least one issue and answers 4xx,
//! and every rejection renders as a response.
#![no_main]

use axum::extract::FromRequestParts;
use axum::http::Request;
use axum::response::IntoResponse;
use id_effect::schema::HasSchema;
use id_effect_axum::route::{RouteRejection, ValidQuery};
use id_effect_fuzz::{Labels, Search, block_on, uri_text};
use libfuzzer_sys::fuzz_target;

fn decode<T: HasSchema>(uri: &str) {
  let Ok(request) = Request::builder().uri(uri).body(()) else {
    return;
  };
  let (mut parts, ()) = request.into_parts();
  if let Err(rejection) = block_on(ValidQuery::<T>::from_request_parts(&mut parts, &())) {
    if let RouteRejection::Invalid(problem) = &rejection {
      assert!(!problem.errors.is_empty(), "a problem without an issue");
      assert!(problem.status.is_client_error(), "{}", problem.status);
    }
    let _ = rejection.into_response();
  }
}

fuzz_target!(|data: &[u8]| {
  let Some((&selector, query)) = data.split_first() else {
    return;
  };
  let uri = format!("/search?{}", uri_text(query));
  if selector % 2 == 0 {
    decode::<Search>(&uri);
  } else {
    decode::<Labels>(&uri);
  }
});
