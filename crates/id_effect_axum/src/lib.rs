//! Run [`id_effect::Effect`] programs inside [Axum](https://docs.rs/axum) handlers and Tower
//! services (the 0.7 kernel).
//!
//! ## Mental model
//!
//! - Axum drives request handling as `async` futures on Tokio.
//! - Your domain stays in `Effect<A, E, R>`. The router state `S` is the effect's environment:
//!   plain clone-cheap state (`Arc` fields), a typed capability list (`HCons<Db, HCons<Clock,
//!   HNil>>`, read with `need::<Db>()`) or a runtime-checked [`DynEnv`].
//! - Every entry point ([`crate::routing`], [`execute`], [`run_with_env`], [`run_with_caps`], the
//!   request-cap helpers and the `service` module) builds the effect with `build(&mut state)`,
//!   provides it with that state and runs it as **one fiber per request** on the tokio runtime
//!   serving the request (see [`executor`]).
//!
//! ## Execution
//!
//! Effects are [`Send`], so there is no bridge any more: no thread pool and no thread
//! parking. Handler futures are [`Send`] and work on any runtime flavour.
//!
//! - **Interruption.** Dropping the handler future (client disconnect, timeout layer, shutdown)
//!   interrupts the request fiber; its finalizers run ([`DropPolicy::Interrupt`], default).
//!   [`DropPolicy::Detach`] lets it finish instead.
//! - **Outcomes.** [`exit_response`] maps the fiber's [`Exit`]: success → `A::into_response`, a
//!   typed failure → `E::into_response`, a defect (a panic anywhere in the build closure or the
//!   effect) → `500 Internal Server Error` with a [`HandlerDefect`] response extension (the
//!   message never reaches the client), an interrupt → `503 Service Unavailable`.
//! - **Backpressure** is opt-in ([`EffectExecutorBuilder::max_in_flight`]); with
//!   [`SaturationPolicy::Reject`] the routing helpers, [`execute`], [`execute_with_request_caps`]
//!   and [`exchange_into_response`] answer `503` ([`Saturated`]).
//!
//! Configure the global executor with [`EffectExecutor::install_global`] before serving.
//!
//! ## Request-scoped capabilities
//!
//! [`CapOverrideLayer`] resolves per-request capability values (tenant, db handle, …) and
//! [`execute_with_request_caps`] / [`run_with_request_caps`] overlay them on the router's base
//! [`DynEnv`] for that request's effect only — see [`request_caps`].
//!
//! ## Quick start
//!
//! ```rust
//! use axum::Router;
//! use id_effect::{Effect, succeed};
//! use id_effect_axum::routing;
//!
//! #[derive(Clone)]
//! struct AppState {
//!   greeting: &'static str,
//! }
//!
//! fn hello(env: &mut AppState) -> Effect<String, std::convert::Infallible, AppState> {
//!   succeed(env.greeting.to_string())
//! }
//!
//! let app: Router = Router::new()
//!   .route("/hello", routing::get(hello))
//!   .with_state(AppState { greeting: "ok" });
//! # let _ = app;
//! ```
//!
//! For full control (extra extractors, middleware ordering), use [`execute`].
//!
//! ## Bodies, JSON and the server
//!
//! - [`body`]: stream a response from a [`Stream`](id_effect::Stream) of `Bytes`
//!   ([`body::stream_body`]) and read a request body as one ([`body::body_stream`]).
//! - [`json::decode_json_schema`] validates JSON with an [`id_effect::schema::Schema`]; schema
//!   errors become **422** with `path` + `message`.
//! - [`server`]: [`HostConfig::layer`] loads the bind settings through `id_effect_config`, and
//!   [`server_layer`] is the HTTP server as a scoped `Layer` (bound on acquire, gracefully shut
//!   down on release); [`serve`] / [`serve_until_shutdown`] launch it.
//!
//! ## Examples
//!
//! See `examples/` (e.g. `cargo run -p id_effect_axum --example 010_routing_hello`).

#![forbid(unsafe_code)]
#![deny(missing_docs)]
// Axum’s `Handler` trait and tower stack are `async fn` at the wire edge; domain logic stays in
// `Effect` via [`crate::routing`] and [`execute`].
#![allow(unknown_lints)]
#![allow(effect_no_async_fn_application)]

pub mod body;
pub mod channel_bridge;
#[cfg(feature = "cluster")]
pub mod cluster;
pub mod executor;
pub mod health;
pub mod json;
pub mod metrics;
pub mod request_caps;
pub mod routing;
pub mod server;
#[cfg(feature = "tower")]
pub mod service;

pub use channel_bridge::{exchange, exchange_into_response};
pub use executor::{
  DropPolicy, EffectExecutor, EffectExecutorBuilder, Saturated, SaturationPolicy,
};
pub use request_caps::{
  CapOverride, CapOverrideLayer, RequestCaps, execute_with_request_caps, run_with_request_caps,
};
pub use server::{
  CSRF_HEADER, ContentSecurityPolicy, CsrfConfig, Host, HostBuilder, HostConfig, HostDrain,
  HostError, ServerHandle, ShutdownReason, bootstrap_env, csp_middleware, csrf_middleware,
  drain_with_timeout, load_host_config, provide_host_config_env, serve, serve_router,
  serve_until_shutdown, server_layer, set_csrf_header, socket_addr, wait_for_shutdown,
};

pub use health::{
  CheckReport, HealthIndicator, HealthOutcome, HealthRegistry, HealthReport, HealthStatus,
  ReadinessCheck, ReadinessState, health, health_router, observability_routes,
  observability_routes_with_state, readiness_router, ready, ready_with_state, require_ready,
};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use id_effect::{Cause, Exit};
use id_effect::{DynEnv, Effect};

/// Run `build` with the runtime-checked capability state [`State<DynEnv>`](axum::extract::State).
#[inline]
pub async fn run_with_caps<A, E, F>(State(env): State<DynEnv>, build: F) -> Exit<A, E>
where
  A: Send + 'static,
  E: Send + 'static,
  F: FnOnce(&mut DynEnv) -> Effect<A, E, DynEnv> + Send + 'static,
{
  run_with_env(env, build).await
}

/// Run `build(&mut env)` to obtain an effect, provide it with the **same** `env` and run it as
/// one request fiber on the global [`EffectExecutor`]; returns the fiber's [`Exit`].
///
/// The future is [`Send`]; dropping it interrupts the fiber (under the default
/// [`DropPolicy::Interrupt`]). Use this when you want the outcome rather than a response.
#[inline]
pub async fn run_with_env<S, A, E, F>(env: S, build: F) -> Exit<A, E>
where
  S: Send + Sync + 'static,
  A: Send + 'static,
  E: Send + 'static,
  F: FnOnce(&mut S) -> Effect<A, E, S> + Send + 'static,
{
  EffectExecutor::global().run(env, build).await
}

/// Like [`run_with_env`], honouring the global executor's [`SaturationPolicy`].
#[inline]
pub(crate) async fn try_run_effect_from_state<S, A, E, F>(
  env: S,
  build: F,
) -> Result<Exit<A, E>, Saturated>
where
  S: Send + Sync + 'static,
  A: Send + 'static,
  E: Send + 'static,
  F: FnOnce(&mut S) -> Effect<A, E, S> + Send + 'static,
{
  EffectExecutor::global().try_run(env, build).await
}

/// Response extension set on the `500` that [`exit_response`] returns for a defect: the defect's
/// message, for logging / tracing middleware. It is never written to the response body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HandlerDefect {
  /// The defect message (a panic's payload text, or the `die(..)` message).
  pub message: String,
}

/// The first typed failure in `cause` (depth-first, left to right), or the cause without one.
pub(crate) fn split_cause<E>(cause: Cause<E>) -> Result<E, Cause<E>> {
  match cause {
    Cause::Fail(e) => Ok(e),
    Cause::Both(l, r) => match split_cause(*l) {
      Ok(e) => Ok(e),
      Err(l) => split_cause(*r).map_err(|r| Cause::Both(Box::new(l), Box::new(r))),
    },
    Cause::Then(l, r) => match split_cause(*l) {
      Ok(e) => Ok(e),
      Err(l) => split_cause(*r).map_err(|r| Cause::Then(Box::new(l), Box::new(r))),
    },
    other => Err(other),
  }
}

/// The first defect message in `cause`, if any.
pub(crate) fn first_defect<E>(cause: &Cause<E>) -> Option<&str> {
  match cause {
    Cause::Die(d) => Some(d.message()),
    Cause::Both(l, r) | Cause::Then(l, r) => first_defect(l).or_else(|| first_defect(r)),
    Cause::Fail(_) | Cause::Interrupt(_) => None,
  }
}

/// The response for a cause without a typed failure: `500` (with a [`HandlerDefect`] extension)
/// when it holds a defect, otherwise (interrupted) `503`.
pub fn cause_response<E>(cause: &Cause<E>) -> Response {
  match first_defect(cause) {
    Some(message) => {
      let mut res = (StatusCode::INTERNAL_SERVER_ERROR, "internal server error").into_response();
      res.extensions_mut().insert(HandlerDefect {
        message: message.to_owned(),
      });
      res
    }
    None => (StatusCode::SERVICE_UNAVAILABLE, "request interrupted").into_response(),
  }
}

/// Map a request fiber's [`Exit`] to a response: success → `A`, the first typed failure → `E`,
/// otherwise [`cause_response`] (defect → `500`, interrupt → `503`).
pub fn exit_response<A: IntoResponse, E: IntoResponse>(exit: Exit<A, E>) -> Response {
  match exit {
    Exit::Success(a) => a.into_response(),
    Exit::Failure(cause) => match split_cause(cause) {
      Ok(e) => e.into_response(),
      Err(rest) => cause_response(&rest),
    },
  }
}

/// Map an executor outcome to a response (`Saturated` → `503`).
#[inline]
pub(crate) fn respond<A: IntoResponse, E: IntoResponse>(
  outcome: Result<Exit<A, E>, Saturated>,
) -> Response {
  match outcome {
    Ok(exit) => exit_response(exit),
    Err(saturated) => saturated.into_response(),
  }
}

/// Axum handler: extract [`State`]`<S>`, build an effect, run it as a request fiber and map the
/// exit through [`exit_response`].
///
/// Answers `503` when the global executor is saturated and configured with
/// [`SaturationPolicy::Reject`].
#[inline]
pub async fn execute<S, A, E, F>(State(env): State<S>, build: F) -> Response
where
  S: Send + Sync + 'static,
  A: IntoResponse + Send + 'static,
  E: IntoResponse + Send + 'static,
  F: FnOnce(&mut S) -> Effect<A, E, S> + Send + 'static,
{
  respond(try_run_effect_from_state(env, build).await)
}

#[cfg(test)]
mod tests {
  use super::*;
  use axum::body::Body;
  use axum::http::{Request, StatusCode};
  use axum::routing::Router;
  use id_effect::{Boundaries, Counter, Histogram, die, fail, succeed, sync};
  use std::convert::Infallible;
  use tower::ServiceExt;

  #[derive(Clone, Default)]
  struct AppState {
    counter: std::sync::Arc<std::sync::atomic::AtomicU32>,
  }

  async fn body_text(res: Response) -> String {
    let bytes = http_body_util::BodyExt::collect(res.into_body())
      .await
      .unwrap()
      .to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
  }

  #[tokio::test]
  async fn routing_get_succeeds() {
    let app = Router::new()
      .route(
        "/",
        crate::routing::get(|s: &mut AppState| {
          s.counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
          succeed::<_, Infallible, _>("hi".to_string())
        }),
      )
      .with_state(AppState::default());

    let req = Request::builder().uri("/").body(Body::empty()).unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
  }

  #[tokio::test]
  async fn a_panicking_handler_is_a_defect_answered_with_500() {
    #[derive(Clone)]
    struct St;
    let app = Router::new()
      .route(
        "/build",
        crate::routing::get(|_: &mut St| -> Effect<String, Infallible, St> {
          panic!("secret detail in build")
        }),
      )
      .route(
        "/effect",
        crate::routing::get(|_: &mut St| {
          sync::<String, Infallible, St>(|| panic!("secret detail in effect"))
        }),
      )
      .route(
        "/die",
        crate::routing::get(|_: &mut St| die::<String, Infallible, St>("explicit defect")),
      )
      .with_state(St);
    for (uri, message) in [
      ("/build", "secret detail in build"),
      ("/effect", "secret detail in effect"),
      ("/die", "explicit defect"),
    ] {
      let req = Request::builder().uri(uri).body(Body::empty()).unwrap();
      let res = app.clone().oneshot(req).await.unwrap();
      assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR, "{uri}");
      assert_eq!(
        res
          .extensions()
          .get::<HandlerDefect>()
          .map(|d| d.message.as_str()),
        Some(message)
      );
      let text = body_text(res).await;
      assert!(
        !text.contains("secret"),
        "defect leaked to the client: {text}"
      );
    }
    // The router keeps serving after a defect.
    let app = Router::new()
      .route(
        "/ok",
        crate::routing::get(|_: &mut St| succeed::<_, Infallible, _>("ok")),
      )
      .with_state(St);
    let res = app
      .oneshot(Request::builder().uri("/ok").body(Body::empty()).unwrap())
      .await
      .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
  }

  #[test]
  fn exit_response_maps_every_cause_shape() {
    let ok = exit_response::<&str, StatusCode>(Exit::Success("a"));
    assert_eq!(ok.status(), StatusCode::OK);
    let typed = exit_response::<&str, StatusCode>(Exit::Failure(Cause::Both(
      Box::new(Cause::die("x")),
      Box::new(Cause::Fail(StatusCode::NOT_FOUND)),
    )));
    assert_eq!(typed.status(), StatusCode::NOT_FOUND);
    let then = exit_response::<&str, StatusCode>(Exit::Failure(Cause::then(
      Cause::die("first"),
      Cause::Fail(StatusCode::CONFLICT),
    )));
    assert_eq!(then.status(), StatusCode::CONFLICT);
    let defect = exit_response::<&str, StatusCode>(Exit::Failure(Cause::then(
      Cause::Fail(StatusCode::IM_A_TEAPOT),
      Cause::die("late"),
    )));
    assert_eq!(defect.status(), StatusCode::IM_A_TEAPOT);
    let only_die = exit_response::<&str, StatusCode>(Exit::Failure(Cause::Both(
      Box::new(Cause::die("first")),
      Box::new(Cause::die("second")),
    )));
    assert_eq!(only_die.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
      only_die
        .extensions()
        .get::<HandlerDefect>()
        .unwrap()
        .message,
      "first"
    );
    let fiber = id_effect::Runtime::global()
      .fork(succeed::<(), (), id_effect::HNil>(()))
      .id();
    let interrupted = exit_response::<&str, StatusCode>(Exit::Failure(Cause::interrupt(fiber)));
    assert_eq!(interrupted.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(interrupted.extensions().get::<HandlerDefect>().is_none());
  }

  #[tokio::test]
  async fn dropping_the_request_future_interrupts_the_handler_fiber() {
    use std::sync::Mutex;
    use std::time::Duration;
    use tokio::sync::oneshot;

    #[derive(Clone)]
    struct St {
      started: std::sync::Arc<Mutex<Option<oneshot::Sender<()>>>>,
      released: std::sync::Arc<Mutex<Option<oneshot::Sender<()>>>>,
    }
    let (started_tx, started_rx) = oneshot::channel();
    let (released_tx, released_rx) = oneshot::channel();
    let st = St {
      started: std::sync::Arc::new(Mutex::new(Some(started_tx))),
      released: std::sync::Arc::new(Mutex::new(Some(released_tx))),
    };
    let app = Router::new()
      .route(
        "/slow",
        crate::routing::get(|s: &mut St| {
          let started = s.started.lock().unwrap().take();
          let released = s.released.lock().unwrap().take();
          sync::<(), Infallible, St>(move || {
            if let Some(tx) = started {
              let _ = tx.send(());
            }
          })
          .then(id_effect::sleep(Duration::from_secs(3600)))
          .map(|()| "never")
          .ensuring(sync(move || {
            if let Some(tx) = released {
              let _ = tx.send(());
            }
          }))
        }),
      )
      .with_state(st);
    let req = Request::builder().uri("/slow").body(Body::empty()).unwrap();
    let in_flight = tokio::spawn(app.oneshot(req));
    started_rx.await.unwrap();
    in_flight.abort(); // the client went away
    tokio::time::timeout(Duration::from_secs(10), released_rx)
      .await
      .expect("the handler's finalizer runs after the request future is dropped")
      .unwrap();
  }

  #[tokio::test]
  async fn typed_failure_uses_the_error_response() {
    #[derive(Clone)]
    struct St;
    let app = Router::new()
      .route(
        "/",
        crate::routing::get(|_: &mut St| {
          fail::<(), (StatusCode, &'static str), St>((StatusCode::CONFLICT, "taken"))
        }),
      )
      .with_state(St);
    let res = app
      .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
      .await
      .unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);
    assert_eq!(body_text(res).await, "taken");
  }

  #[tokio::test]
  async fn axum_route_counter_increments_per_request() {
    let ctr = Counter::new("id_effect_axum_lib_requests");
    let hist = Histogram::new(
      "id_effect_axum_lib_latency",
      Boundaries::exponential(0.0001, 2.0, 16),
    );

    #[derive(Clone, Default)]
    struct St;

    let app = Router::new()
      .route(
        "/",
        crate::routing::get_with_metrics(ctr.clone(), hist.clone(), |_s: &mut St| {
          succeed::<_, Infallible, _>("ok".to_string())
        }),
      )
      .with_state(St);

    for _ in 0..2 {
      let req = Request::builder().uri("/").body(Body::empty()).unwrap();
      let res = app.clone().oneshot(req).await.unwrap();
      assert_eq!(res.status(), StatusCode::OK);
    }

    assert_eq!(ctr.count(), 2);
    assert_eq!(hist.snapshot().count, 2);
  }

  #[tokio::test]
  async fn axum_channel_handler_responds_to_request() {
    use axum::body::Bytes;
    use axum::http::Response as HttpResponse;
    use id_effect::{Channel, Queue};

    #[derive(Clone)]
    struct St {
      ch: Channel<HttpResponse<Bytes>, Request<Bytes>>,
    }

    let ch = Channel::from_queue_and_map(Queue::unbounded(), |req: Request<Bytes>| {
      HttpResponse::new(req.into_body())
    });
    let app = Router::new()
      .route(
        "/",
        axum::routing::get(|State(s): State<St>, req: Request<Body>| async move {
          exchange_into_response(s.ch, req).await
        }),
      )
      .with_state(St { ch });

    let req = Request::builder()
      .uri("/")
      .body(Body::from("ping"))
      .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(body_text(res).await, "ping");
  }

  #[tokio::test]
  async fn per_request_dyn_env_mutation_stays_local() {
    #[derive(Clone, Copy)]
    struct RequestId(u32);

    let base = DynEnv::new().with(RequestId(0));

    let app = Router::new()
      .route(
        "/",
        crate::routing::get(|env: &mut DynEnv| {
          env.insert(RequestId(42));
          let id = env.try_get::<RequestId>().map_or(0, |r| r.0);
          succeed::<_, Infallible, _>(id.to_string())
        }),
      )
      .with_state(base.clone());

    let req = Request::builder().uri("/").body(Body::empty()).unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(body_text(res).await, "42");
    assert_eq!(base.try_get::<RequestId>().unwrap().0, 0);
  }

  #[tokio::test]
  async fn typed_capability_state_reads_with_need() {
    use id_effect::{HCons, HNil, hlist, need};

    struct Greeting(&'static str);
    type AppEnv = HCons<Greeting, HNil>;

    fn hello(_: &mut AppEnv) -> Effect<String, Infallible, AppEnv> {
      need::<Greeting>().map(|g| g.0.to_string())
    }

    let app = Router::new()
      .route("/", crate::routing::get(hello))
      .with_state(hlist![Greeting("typed")]);
    let res = app
      .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
      .await
      .unwrap();
    assert_eq!(body_text(res).await, "typed");
  }

  #[tokio::test]
  async fn run_with_env_and_caps_helpers() {
    #[derive(Clone, Default)]
    struct St;
    let out = run_with_env(St, |s: &mut St| {
      let _ = s;
      succeed::<_, Infallible, _>(11_u32)
    })
    .await;
    assert_eq!(out, Exit::Success(11));
    let out2 = run_with_caps(State(DynEnv::new()), |_e: &mut DynEnv| {
      succeed::<_, Infallible, _>(22_u32)
    })
    .await;
    assert_eq!(out2, Exit::Success(22));
  }
}
