//! Form-body decode (id_effect_axum `route::ValidForm`, the decoder typed routes share): the
//! first byte picks `application/x-www-form-urlencoded`, `multipart/form-data` (boundary `XyZ`),
//! a `GET` whose query string is the form, and the schema; the rest is the body.
//!
//! Checks besides "no panic": an `Invalid` rejection lists at least one issue and answers 4xx,
//! and every rejection renders as a response.
#![no_main]

use axum::body::Body;
use axum::extract::FromRequest;
use axum::http::{Method, Request, header};
use axum::response::IntoResponse;
use id_effect::schema::HasSchema;
use id_effect_axum::route::{RouteRejection, ValidForm};
use id_effect_fuzz::{Labels, Search, block_on, uri_text};
use libfuzzer_sys::fuzz_target;

fn decode<T: HasSchema>(request: Request<Body>) {
  if let Err(rejection) = block_on(ValidForm::<T>::from_request(request, &())) {
    if let RouteRejection::Invalid(problem) = &rejection {
      assert!(!problem.errors.is_empty(), "a problem without an issue");
      assert!(problem.status.is_client_error(), "{}", problem.status);
    }
    let _ = rejection.into_response();
  }
}

fuzz_target!(|data: &[u8]| {
  let Some((&selector, body)) = data.split_first() else {
    return;
  };
  let request = match selector % 3 {
    0 => Request::builder()
      .method(Method::POST)
      .uri("/form")
      .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
      .body(Body::from(body.to_vec())),
    1 => Request::builder()
      .method(Method::POST)
      .uri("/form")
      .header(header::CONTENT_TYPE, "multipart/form-data; boundary=XyZ")
      .body(Body::from(body.to_vec())),
    _ => Request::builder()
      .method(Method::GET)
      .uri(format!("/form?{}", uri_text(body)))
      .body(Body::empty()),
  };
  let Ok(request) = request else {
    return;
  };
  if selector & 0x80 == 0 {
    decode::<Search>(request);
  } else {
    decode::<Labels>(request);
  }
});
