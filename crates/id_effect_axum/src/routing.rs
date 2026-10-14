//! Method routers that wrap `Effect<A, E, S>` with [`axum::extract::State`].
//!
//! Each helper returns a [`MethodRouter`] you can pass to
//! [`axum::Router::route`].
//!
//! [`get_with_metrics`] (and siblings) increment a request [`Metric`]
//! counter and record handler wall time in a latency [`Metric`] (histogram / summary / timer) via
//! [`id_effect::Metric::track_duration`].
//!
//! Effects run on the global [`crate::EffectExecutor`]; when it is saturated and configured with
//! [`crate::SaturationPolicy::Reject`], handlers answer `503` ([`crate::Saturated`]).

use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::{MethodFilter, MethodRouter};
use id_effect::Effect;
use id_effect::Metric;
use id_effect::runtime::run_blocking;
use id_effect::scheduling::duration::Duration;

async fn run_with_axum_metrics<S, A, E, F>(
  env: S,
  request_counter: Metric<u64, ()>,
  latency: Metric<Duration, ()>,
  f: F,
) -> Result<Result<A, E>, crate::Saturated>
where
  S: Send + 'static,
  A: Send + 'static,
  E: Send + 'static,
  F: FnOnce(&mut S) -> Effect<A, E, S> + Send + 'static,
{
  crate::try_run_effect_from_state(env, move |e| {
    // Counter increments are synchronous; run them on the executor worker with the request.
    run_blocking(request_counter.apply(1), ()).expect("request counter");
    latency.track_duration(f(e))
  })
  .await
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
  axum::routing::get(move |st: State<S>| {
    let f = f.clone();
    async move {
      let State(env) = st;
      crate::respond(crate::try_run_effect_from_state(env, move |e| f(e)).await)
    }
  })
}

/// `GET` with per-route request counting and latency recording.
///
/// Pass counters/histograms (typically tagged with `route`, etc.) built via [`Metric::counter`] /
/// [`Metric::histogram`].
#[inline]
pub fn get_with_metrics<S, A, E, F>(
  request_counter: Metric<u64, ()>,
  latency: Metric<Duration, ()>,
  f: F,
) -> MethodRouter<S>
where
  S: Clone + Send + Sync + 'static,
  A: IntoResponse + Send + 'static,
  E: IntoResponse + Send + 'static,
  F: Fn(&mut S) -> Effect<A, E, S> + Clone + Send + Sync + 'static,
{
  axum::routing::get(move |st: State<S>| {
    let f = f.clone();
    let ctr = request_counter.clone();
    let lat = latency.clone();
    async move {
      let State(env) = st;
      crate::respond(run_with_axum_metrics(env, ctr, lat, move |e| f(e)).await)
    }
  })
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
  axum::routing::post(move |st: State<S>| {
    let f = f.clone();
    async move {
      let State(env) = st;
      crate::respond(crate::try_run_effect_from_state(env, move |e| f(e)).await)
    }
  })
}

/// `POST` with request counter + latency [`Metric`] (see [`get_with_metrics`]).
#[inline]
pub fn post_with_metrics<S, A, E, F>(
  request_counter: Metric<u64, ()>,
  latency: Metric<Duration, ()>,
  f: F,
) -> MethodRouter<S>
where
  S: Clone + Send + Sync + 'static,
  A: IntoResponse + Send + 'static,
  E: IntoResponse + Send + 'static,
  F: Fn(&mut S) -> Effect<A, E, S> + Clone + Send + Sync + 'static,
{
  axum::routing::post(move |st: State<S>| {
    let f = f.clone();
    let ctr = request_counter.clone();
    let lat = latency.clone();
    async move {
      let State(env) = st;
      crate::respond(run_with_axum_metrics(env, ctr, lat, move |e| f(e)).await)
    }
  })
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
  axum::routing::put(move |st: State<S>| {
    let f = f.clone();
    async move {
      let State(env) = st;
      crate::respond(crate::try_run_effect_from_state(env, move |e| f(e)).await)
    }
  })
}

/// `PUT` with request counter + latency [`Metric`].
#[inline]
pub fn put_with_metrics<S, A, E, F>(
  request_counter: Metric<u64, ()>,
  latency: Metric<Duration, ()>,
  f: F,
) -> MethodRouter<S>
where
  S: Clone + Send + Sync + 'static,
  A: IntoResponse + Send + 'static,
  E: IntoResponse + Send + 'static,
  F: Fn(&mut S) -> Effect<A, E, S> + Clone + Send + Sync + 'static,
{
  axum::routing::put(move |st: State<S>| {
    let f = f.clone();
    let ctr = request_counter.clone();
    let lat = latency.clone();
    async move {
      let State(env) = st;
      crate::respond(run_with_axum_metrics(env, ctr, lat, move |e| f(e)).await)
    }
  })
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
  axum::routing::patch(move |st: State<S>| {
    let f = f.clone();
    async move {
      let State(env) = st;
      crate::respond(crate::try_run_effect_from_state(env, move |e| f(e)).await)
    }
  })
}

/// `PATCH` with request counter + latency [`Metric`].
#[inline]
pub fn patch_with_metrics<S, A, E, F>(
  request_counter: Metric<u64, ()>,
  latency: Metric<Duration, ()>,
  f: F,
) -> MethodRouter<S>
where
  S: Clone + Send + Sync + 'static,
  A: IntoResponse + Send + 'static,
  E: IntoResponse + Send + 'static,
  F: Fn(&mut S) -> Effect<A, E, S> + Clone + Send + Sync + 'static,
{
  axum::routing::patch(move |st: State<S>| {
    let f = f.clone();
    let ctr = request_counter.clone();
    let lat = latency.clone();
    async move {
      let State(env) = st;
      crate::respond(run_with_axum_metrics(env, ctr, lat, move |e| f(e)).await)
    }
  })
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
  axum::routing::delete(move |st: State<S>| {
    let f = f.clone();
    async move {
      let State(env) = st;
      crate::respond(crate::try_run_effect_from_state(env, move |e| f(e)).await)
    }
  })
}

/// `DELETE` with request counter + latency [`Metric`].
#[inline]
pub fn delete_with_metrics<S, A, E, F>(
  request_counter: Metric<u64, ()>,
  latency: Metric<Duration, ()>,
  f: F,
) -> MethodRouter<S>
where
  S: Clone + Send + Sync + 'static,
  A: IntoResponse + Send + 'static,
  E: IntoResponse + Send + 'static,
  F: Fn(&mut S) -> Effect<A, E, S> + Clone + Send + Sync + 'static,
{
  axum::routing::delete(move |st: State<S>| {
    let f = f.clone();
    let ctr = request_counter.clone();
    let lat = latency.clone();
    async move {
      let State(env) = st;
      crate::respond(run_with_axum_metrics(env, ctr, lat, move |e| f(e)).await)
    }
  })
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
  axum::routing::on(method, move |st: State<S>| {
    let f = f.clone();
    async move {
      let State(env) = st;
      crate::respond(crate::try_run_effect_from_state(env, move |e| f(e)).await)
    }
  })
}

/// Custom method filter with request counter + latency [`Metric`].
#[inline]
pub fn on_with_metrics<S, A, E, F>(
  method: MethodFilter,
  request_counter: Metric<u64, ()>,
  latency: Metric<Duration, ()>,
  f: F,
) -> MethodRouter<S>
where
  S: Clone + Send + Sync + 'static,
  A: IntoResponse + Send + 'static,
  E: IntoResponse + Send + 'static,
  F: Fn(&mut S) -> Effect<A, E, S> + Clone + Send + Sync + 'static,
{
  axum::routing::on(method, move |st: State<S>| {
    let f = f.clone();
    let ctr = request_counter.clone();
    let lat = latency.clone();
    async move {
      let State(env) = st;
      crate::respond(run_with_axum_metrics(env, ctr, lat, move |e| f(e)).await)
    }
  })
}

#[cfg(test)]
mod tests {
  use std::convert::Infallible;

  use axum::body::Body;
  use axum::http::{Method, Request, StatusCode};
  use axum::routing::{MethodFilter, Router};
  use id_effect::scheduling::duration::Duration;
  use id_effect::{Effect, Metric, fail, succeed};
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

  #[tokio::test]
  async fn get_post_put_patch_delete_and_error_paths() {
    let app = Router::new()
      .route("/g", get(ok))
      .route("/p", post(ok))
      .route("/u", put(ok))
      .route("/a", patch(ok))
      .route("/d", delete(ok))
      .route("/e", get(fail_handler))
      .with_state(AppState(()));

    for (method, path) in [
      (Method::GET, "/g"),
      (Method::POST, "/p"),
      (Method::PUT, "/u"),
      (Method::PATCH, "/a"),
      (Method::DELETE, "/d"),
    ] {
      let res = app
        .clone()
        .oneshot(
          Request::builder()
            .method(method)
            .uri(path)
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
      assert_eq!(res.status(), StatusCode::OK);
    }

    let res = app
      .oneshot(Request::builder().uri("/e").body(Body::empty()).unwrap())
      .await
      .unwrap();
    assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
  }

  #[tokio::test]
  async fn on_and_metrics_variants_execute() {
    let ctr = Metric::counter("c", []);
    let lat = Metric::<Duration, ()>::histogram("h", []);
    let app = Router::new()
      .route("/gm", get_with_metrics(ctr.clone(), lat.clone(), ok))
      .route("/pm", post_with_metrics(ctr.clone(), lat.clone(), ok))
      .route("/um", put_with_metrics(ctr.clone(), lat.clone(), ok))
      .route("/am", patch_with_metrics(ctr.clone(), lat.clone(), ok))
      .route("/dm", delete_with_metrics(ctr.clone(), lat.clone(), ok))
      .route("/o", on(MethodFilter::OPTIONS, ok))
      .with_state(AppState(()));

    for (method, path) in [
      (Method::GET, "/gm"),
      (Method::POST, "/pm"),
      (Method::PUT, "/um"),
      (Method::PATCH, "/am"),
      (Method::DELETE, "/dm"),
    ] {
      let _ = app
        .clone()
        .oneshot(
          Request::builder()
            .method(method)
            .uri(path)
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    }

    let _ = app
      .clone()
      .oneshot(
        Request::builder()
          .method(Method::OPTIONS)
          .uri("/o")
          .body(Body::empty())
          .unwrap(),
      )
      .await
      .unwrap();

    let app2 = Router::new()
      .route("/h", on_with_metrics(MethodFilter::HEAD, ctr, lat, ok))
      .with_state(AppState(()));
    let _ = app2
      .oneshot(
        Request::builder()
          .method(Method::HEAD)
          .uri("/h")
          .body(Body::empty())
          .unwrap(),
      )
      .await
      .unwrap();
  }
}
