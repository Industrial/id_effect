//! The prelude and the crate re-exports resolve to working items, per feature.

use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use id_effect_kit::id_effect::{HCons, HNil};
use id_effect_kit::prelude::*;

#[derive(Debug, PartialEq)]
struct Boom;

#[derive(Debug, PartialEq)]
struct Other;

struct Greeting(&'static str);

#[test]
fn succeed_and_fail_run_to_their_exit() {
  assert!(matches!(
    succeed::<_, Boom, _>(7).block_on(),
    Exit::Success(7)
  ));
  assert!(matches!(
    fail::<u8, _, _>(Boom).block_on(),
    Exit::Failure(Cause::Fail(Boom))
  ));
}

#[test]
fn err_macro_spells_an_or_channel() {
  let eff: Effect<u8, err!(Boom | Other)> = fail(Or::Right(Other));
  assert!(matches!(
    eff.block_on(),
    Exit::Failure(Cause::Fail(Or::Right(Other)))
  ));
}

#[test]
fn need_reads_a_capability_provided_as_an_hlist() {
  let eff: Effect<String, Boom, HCons<Greeting, HNil>> =
    need::<Greeting>().map(|g| format!("{} kit", g.0));
  let out = eff.provide_env(hlist![Greeting("hello")]).block_on();
  assert!(matches!(out, Exit::Success(s) if s == "hello kit"));
}

#[test]
fn retry_follows_a_schedule() {
  let calls = Arc::new(AtomicU32::new(0));
  let counter = Arc::clone(&calls);
  let eff: Effect<u32, Boom> = Effect::retry(
    move || {
      let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
      if n < 3 { fail(Boom) } else { succeed(n) }
    },
    Schedule::recurs(5),
  );
  assert!(matches!(eff.block_on(), Exit::Success(3)));
  assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[test]
fn from_async_and_sleep_compose() {
  let eff: Effect<u8, Boom> =
    sleep(std::time::Duration::from_millis(1)).flat_map(|()| from_async(|| async { Ok(5) }));
  assert!(matches!(eff.block_on(), Exit::Success(5)));
}

#[test]
fn secret_never_prints_its_value() {
  let token = Secret::new("hunter2".to_string());
  assert_eq!(format!("{token:?}"), "<redacted>");
  assert_eq!(token.expose(), "hunter2");
}

#[test]
fn run_main_maps_success_and_failure_to_exit_codes() {
  let ok = run_main(
    succeed::<_, Boom, _>(()),
    hlist![],
    RunMainConfig::minimal().ctrl_c(false),
  );
  assert_eq!(ok, ExitCode::SUCCESS);
  let failed = run_main(
    fail::<(), _, _>(Boom),
    hlist![],
    RunMainConfig::minimal().ctrl_c(false),
  );
  assert_eq!(failed, ExitCode::from(1));
}

#[test]
fn always_on_crates_are_re_exported_whole() {
  let eff: Effect<u8, Boom> = id_effect_kit::id_effect::succeed(1);
  assert!(matches!(eff.block_on(), Exit::Success(1)));
  let config = id_effect_kit::cli::RunMainConfig::with_tracing();
  assert!(config.install_tracing);
  let secret = id_effect_kit::config::Secret::new(3_u8);
  assert_eq!(secret.into_inner(), 3);
}

#[cfg(feature = "web")]
#[tokio::test]
async fn web_prelude_routes_answer() {
  use axum::body::Body;
  use axum::http::{Request, StatusCode};
  use tower::ServiceExt;

  for (router, path, status) in [
    (health_router(), "/health", StatusCode::OK),
    (
      observability_routes(Arc::new(|| true)).merge(metrics_router()),
      "/metrics",
      StatusCode::OK,
    ),
    (
      readiness_router(Arc::new(|| true)),
      "/ready",
      StatusCode::OK,
    ),
    (
      observability_routes(Arc::new(|| false)),
      "/ready",
      StatusCode::SERVICE_UNAVAILABLE,
    ),
  ] {
    let req = Request::builder().uri(path).body(Body::empty()).unwrap();
    let res = router.oneshot(req).await.unwrap();
    assert_eq!(res.status(), status, "{path}");
  }
}

#[cfg(feature = "web")]
#[test]
fn web_crate_and_executor_are_reachable() {
  let _executor: EffectExecutor = id_effect_kit::axum::EffectExecutor::new();
  assert!(std::any::type_name::<SecurityLayer>().ends_with("SecurityLayer"));
}

#[cfg(feature = "sql-pg")]
#[test]
fn with_transaction_commits_on_success() {
  use id_effect_kit::sql::{SqlClient, TestSqlClient};

  let test = TestSqlClient::new();
  assert!(matches!(test.connect().block_on(), Exit::Success(())));
  let service = SqlClientService::new(test.clone());
  let eff: Effect<u8, SqlError> = with_transaction(Arc::clone(&service.0), |_tx| succeed(9));
  assert!(matches!(eff.block_on(), Exit::Success(9)));
  assert_eq!(test.commits(), 1);
}

#[cfg(feature = "otel")]
#[test]
fn otel_config_defaults_to_a_local_collector() {
  let config = OtelConfig::localhost("kit");
  assert_eq!(config.service_name, "kit");
  let install: fn(OtelConfig) -> _ = install_from_config;
  let _ = install;
  assert!(
    id_effect_kit::otel::OtelConfig::localhost("x")
      .endpoint
      .contains("4317")
  );
}

#[cfg(feature = "cluster")]
#[test]
fn cluster_crate_is_reachable() {
  assert!(std::any::type_name::<id_effect_kit::node::NodeId>().ends_with("NodeId"));
}
