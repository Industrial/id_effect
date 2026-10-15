//! Method routers that run `Effect<A, E, S>` handlers with [`axum::extract::State`]`<S>`.
//!
//! Each helper returns a [`MethodRouter`] you can pass to [`axum::Router::route`]. Per request,
//! the handler `f(&mut state)` builds the effect, which is provided with that state and run as a
//! request fiber on the global [`crate::EffectExecutor`]; the exit is mapped with
//! [`crate::exit_response`] (typed failure → `E`, defect → `500`, interrupt → `503`).
//!
//! [`get_with_metrics`] (and siblings) increment a request [`Counter`] and record the handler's
//! duration (seconds, executor clock) in a latency [`Histogram`] via
//! [`Histogram::track_duration`].
//!
//! When the executor is saturated and configured with [`crate::SaturationPolicy::Reject`],
//! handlers answer `503` ([`crate::Saturated`]).

use std::future::Future;

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::{MethodFilter, MethodRouter};
use id_effect::next::{Counter, Effect, Histogram};

/// The Axum handler behind every helper: one request fiber per call.
fn handler<S, A, E, F>(f: F) -> impl Fn(State<S>) -> HandlerFuture + Clone + Send + Sync + 'static
where
  S: Clone + Send + Sync + 'static,
  A: IntoResponse + Send + 'static,
  E: IntoResponse + Send + 'static,
  F: Fn(&mut S) -> Effect<A, E, S> + Clone + Send + Sync + 'static,
{
  move |State(env): State<S>| {
    let f = f.clone();
    Box::pin(
      async move { crate::respond(crate::try_run_effect_from_state(env, move |e| f(e)).await) },
    ) as HandlerFuture
  }
}

type HandlerFuture = std::pin::Pin<Box<dyn Future<Output = Response> + Send>>;

/// [`handler`] with the request counter and latency histogram around the effect.
fn metered<S, A, E, F>(
  request_counter: Counter,
  latency: Histogram,
  f: F,
) -> impl Fn(State<S>) -> HandlerFuture + Clone + Send + Sync + 'static
where
  S: Clone + Send + Sync + 'static,
  A: IntoResponse + Send + 'static,
  E: IntoResponse + Send + 'static,
  F: Fn(&mut S) -> Effect<A, E, S> + Clone + Send + Sync + 'static,
{
  handler(move |e: &mut S| {
    request_counter
      .increment::<E, S>()
      .then(latency.track_duration(f(e)))
  })
}

/// `GET` — `f` is invoked per request; use [`Clone`] on `f` when the router stores it (e.g. closure
/// with `Arc` captures).
#[inline]
pub fn get<S, A, E, F>(f: F) -> MethodRouter<S>
where
  S: Clone + Send + Sync + 'static,
  A: IntoResponse + Send + 'static,
  E: IntoResponse + Send + 'static,
  F: Fn(&mut S) -> Effect<A, E, S> + Clone + Send + Sync + 'static,
{
  axum::routing::get(handler(f))
}

/// `GET` with per-route request counting and latency recording.
///
/// Pass handles (typically tagged with `route`, etc.) built via [`Counter::new`] /
/// [`Histogram::new`].
#[inline]
pub fn get_with_metrics<S, A, E, F>(
  request_counter: Counter,
  latency: Histogram,
  f: F,
) -> MethodRouter<S>
where
  S: Clone + Send + Sync + 'static,
  A: IntoResponse + Send + 'static,
  E: IntoResponse + Send + 'static,
  F: Fn(&mut S) -> Effect<A, E, S> + Clone + Send + Sync + 'static,
{
  axum::routing::get(metered(request_counter, latency, f))
}

/// `POST`
#[inline]
pub fn post<S, A, E, F>(f: F) -> MethodRouter<S>
where
  S: Clone + Send + Sync + 'static,
  A: IntoResponse + Send + 'static,
  E: IntoResponse + Send + 'static,
  F: Fn(&mut S) -> Effect<A, E, S> + Clone + Send + Sync + 'static,
{
  axum::routing::post(handler(f))
}

/// `POST` with request counter + latency histogram (see [`get_with_metrics`]).
#[inline]
pub fn post_with_metrics<S, A, E, F>(
  request_counter: Counter,
  latency: Histogram,
  f: F,
) -> MethodRouter<S>
where
  S: Clone + Send + Sync + 'static,
  A: IntoResponse + Send + 'static,
  E: IntoResponse + Send + 'static,
  F: Fn(&mut S) -> Effect<A, E, S> + Clone + Send + Sync + 'static,
{
  axum::routing::post(metered(request_counter, latency, f))
}

/// `PUT`
#[inline]
pub fn put<S, A, E, F>(f: F) -> MethodRouter<S>
where
  S: Clone + Send + Sync + 'static,
  A: IntoResponse + Send + 'static,
  E: IntoResponse + Send + 'static,
  F: Fn(&mut S) -> Effect<A, E, S> + Clone + Send + Sync + 'static,
{
  axum::routing::put(handler(f))
}

/// `PUT` with request counter + latency histogram.
#[inline]
pub fn put_with_metrics<S, A, E, F>(
  request_counter: Counter,
  latency: Histogram,
  f: F,
) -> MethodRouter<S>
where
  S: Clone + Send + Sync + 'static,
  A: IntoResponse + Send + 'static,
  E: IntoResponse + Send + 'static,
  F: Fn(&mut S) -> Effect<A, E, S> + Clone + Send + Sync + 'static,
{
  axum::routing::put(metered(request_counter, latency, f))
}

/// `PATCH`
#[inline]
pub fn patch<S, A, E, F>(f: F) -> MethodRouter<S>
where
  S: Clone + Send + Sync + 'static,
  A: IntoResponse + Send + 'static,
  E: IntoResponse + Send + 'static,
  F: Fn(&mut S) -> Effect<A, E, S> + Clone + Send + Sync + 'static,
{
  axum::routing::patch(handler(f))
}

/// `PATCH` with request counter + latency histogram.
#[inline]
pub fn patch_with_metrics<S, A, E, F>(
  request_counter: Counter,
  latency: Histogram,
  f: F,
) -> MethodRouter<S>
where
  S: Clone + Send + Sync + 'static,
  A: IntoResponse + Send + 'static,
  E: IntoResponse + Send + 'static,
  F: Fn(&mut S) -> Effect<A, E, S> + Clone + Send + Sync + 'static,
{
  axum::routing::patch(metered(request_counter, latency, f))
}

/// `DELETE`
#[inline]
pub fn delete<S, A, E, F>(f: F) -> MethodRouter<S>
where
  S: Clone + Send + Sync + 'static,
  A: IntoResponse + Send + 'static,
  E: IntoResponse + Send + 'static,
  F: Fn(&mut S) -> Effect<A, E, S> + Clone + Send + Sync + 'static,
{
  axum::routing::delete(handler(f))
}

/// `DELETE` with request counter + latency histogram.
#[inline]
pub fn delete_with_metrics<S, A, E, F>(
  request_counter: Counter,
  latency: Histogram,
  f: F,
) -> MethodRouter<S>
where
  S: Clone + Send + Sync + 'static,
  A: IntoResponse + Send + 'static,
  E: IntoResponse + Send + 'static,
  F: Fn(&mut S) -> Effect<A, E, S> + Clone + Send + Sync + 'static,
{
  axum::routing::delete(metered(request_counter, latency, f))
}

/// Custom method filter (e.g. [`MethodFilter::HEAD`](MethodFilter::HEAD)).
#[inline]
pub fn on<S, A, E, F>(method: MethodFilter, f: F) -> MethodRouter<S>
where
  S: Clone + Send + Sync + 'static,
  A: IntoResponse + Send + 'static,
  E: IntoResponse + Send + 'static,
  F: Fn(&mut S) -> Effect<A, E, S> + Clone + Send + Sync + 'static,
{
  axum::routing::on(method, handler(f))
}

/// Custom method filter with request counter + latency histogram.
#[inline]
pub fn on_with_metrics<S, A, E, F>(
  method: MethodFilter,
  request_counter: Counter,
  latency: Histogram,
  f: F,
) -> MethodRouter<S>
where
  S: Clone + Send + Sync + 'static,
  A: IntoResponse + Send + 'static,
  E: IntoResponse + Send + 'static,
  F: Fn(&mut S) -> Effect<A, E, S> + Clone + Send + Sync + 'static,
{
  axum::routing::on(method, metered(request_counter, latency, f))
}

#[cfg(test)]
mod tests {
  use std::convert::Infallible;

  use axum::body::Body;
  use axum::http::{Method, Request, StatusCode};
  use axum::routing::{MethodFilter, Router};
  use id_effect::next::{Boundaries, Counter, Effect, Histogram, fail, succeed};
  use tower::ServiceExt;

  use super::*;

  #[derive(Clone)]
  struct AppState(());

  fn ok(_: &mut AppState) -> Effect<&'static str, Infallible, AppState> {
    succeed("ok")
  }

  fn fail_handler(_: &mut AppState) -> Effect<(), (StatusCode, &'static str), AppState> {
    fail((StatusCode::INTERNAL_SERVER_ERROR, "nope"))
  }

  async fn status(app: &Router, method: Method, path: &str) -> StatusCode {
    app
      .clone()
      .oneshot(
        Request::builder()
          .method(method)
          .uri(path)
          .body(Body::empty())
          .unwrap(),
      )
      .await
      .unwrap()
      .status()
  }

  #[tokio::test]
  async fn get_post_put_patch_delete_and_error_paths() {
    let app = Router::new()
      .route("/g", get(ok))
      .route("/p", post(ok))
      .route("/u", put(ok))
      .route("/a", patch(ok))
      .route("/d", delete(ok))
      .route("/h", on(MethodFilter::HEAD, ok))
      .route("/e", get(fail_handler))
      .with_state(AppState(()));

    for (method, path) in [
      (Method::GET, "/g"),
      (Method::POST, "/p"),
      (Method::PUT, "/u"),
      (Method::PATCH, "/a"),
      (Method::DELETE, "/d"),
      (Method::HEAD, "/h"),
    ] {
      assert_eq!(status(&app, method, path).await, StatusCode::OK, "{path}");
    }
    assert_eq!(
      status(&app, Method::GET, "/e").await,
      StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(
      status(&app, Method::POST, "/g").await,
      StatusCode::METHOD_NOT_ALLOWED
    );
  }

  #[tokio::test]
  async fn every_metered_helper_counts_and_times() {
    let ctr = Counter::new("id_effect_axum_routing_requests");
    let lat = Histogram::new(
      "id_effect_axum_routing_latency",
      Boundaries::exponential(0.0001, 2.0, 16),
    );
    let app = Router::new()
      .route("/g", get_with_metrics(ctr.clone(), lat.clone(), ok))
      .route("/p", post_with_metrics(ctr.clone(), lat.clone(), ok))
      .route("/u", put_with_metrics(ctr.clone(), lat.clone(), ok))
      .route("/a", patch_with_metrics(ctr.clone(), lat.clone(), ok))
      .route("/d", delete_with_metrics(ctr.clone(), lat.clone(), ok))
      .route(
        "/h",
        on_with_metrics(MethodFilter::HEAD, ctr.clone(), lat.clone(), ok),
      )
      .route(
        "/e",
        get_with_metrics(ctr.clone(), lat.clone(), fail_handler),
      )
      .with_state(AppState(()));
    for (method, path) in [
      (Method::GET, "/g"),
      (Method::POST, "/p"),
      (Method::PUT, "/u"),
      (Method::PATCH, "/a"),
      (Method::DELETE, "/d"),
      (Method::HEAD, "/h"),
      (Method::GET, "/e"),
    ] {
      let _ = status(&app, method, path).await;
    }
    assert_eq!(ctr.count(), 7);
    // Failures are timed too.
    assert_eq!(lat.snapshot().count, 7);
  }
}
