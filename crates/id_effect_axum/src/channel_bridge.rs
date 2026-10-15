//! Bridge a queue-backed [`Channel`] to Axum: one HTTP exchange is
//! `write(Request)` then `read()` for the response.
//!
//! The channel carries [`Bytes`] bodies (`http::Request<Bytes>` / `http::Response<Bytes>`); use
//! [`exchange_into_response`] with Axum's streaming [`Body`] (the request body is buffered before
//! the round trip). A read on a shut-down, drained channel maps to `503`.

use axum::body::{Body, Bytes};
use axum::http::{Request, Response, StatusCode};
use axum::response::{IntoResponse, Response as AxumResponse};
use http_body_util::BodyExt;
use id_effect::{Channel, Effect, fail, succeed};

/// Build an effect that performs one round trip on `ch`: enqueue `req`, then await the mapped
/// response. A read that finds the channel shut down fails with `E::from(503)`.
pub fn exchange<A, E, R>(
  ch: Channel<Response<Bytes>, Request<Bytes>>,
  req: Request<Bytes>,
) -> Effect<A, E, R>
where
  A: From<Response<Bytes>> + Send + 'static,
  E: From<StatusCode> + Send + 'static,
  R: Send + Sync + 'static,
{
  ch.write::<E, R>(req)
    .then(ch.read())
    .flat_map(|resp| match resp {
      Some(resp) => succeed(resp.into()),
      None => fail(StatusCode::SERVICE_UNAVAILABLE.into()),
    })
}

/// Buffer `req`'s body to [`Bytes`], run [`exchange`] as a request fiber, then map the wire
/// response to Axum's [`Body`] (`503` when the global executor is saturated under
/// [`SaturationPolicy::Reject`](crate::SaturationPolicy::Reject)).
pub async fn exchange_into_response(
  ch: Channel<Response<Bytes>, Request<Bytes>>,
  req: Request<Body>,
) -> AxumResponse {
  let (parts, body) = req.into_parts();
  let bytes = match BodyExt::collect(body).await {
    Ok(c) => c.to_bytes(),
    Err(_) => return StatusCode::BAD_REQUEST.into_response(),
  };
  let req_b = Request::from_parts(parts, bytes);
  let outcome = crate::try_run_effect_from_state((), move |_: &mut ()| {
    exchange::<Response<Bytes>, StatusCode, ()>(ch, req_b)
  })
  .await
  .map(|exit| {
    exit.map(|r| {
      let (parts, b) = r.into_parts();
      AxumResponse::from_parts(parts, Body::from(b))
    })
  });
  crate::respond(outcome)
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect::Exit;
  use id_effect::Queue;

  #[tokio::test]
  async fn shut_down_channel_answers_503() {
    let ch = Channel::from_queue_and_map(Queue::unbounded(), |r: Request<Bytes>| {
      Response::new(r.into_body())
    });
    let shut = crate::run_with_env((), {
      let ch = ch.clone();
      move |_: &mut ()| ch.shutdown::<std::convert::Infallible, ()>()
    })
    .await;
    assert_eq!(shut, Exit::Success(()));
    let res = exchange_into_response(ch, Request::new(Body::from("x"))).await;
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
  }
}
