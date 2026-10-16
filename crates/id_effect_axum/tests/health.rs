//! Aggregated readiness ([`HealthRegistry`]) and the readiness flip on shutdown.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::get;
use http_body_util::BodyExt;
use id_effect::{Cache, Effect, die, fail, sleep, succeed};
use id_effect_axum::{
  HealthIndicator, HealthOutcome, HealthRegistry, HealthStatus, HostConfig, ReadinessCheck,
  ReadinessState, ServerHandle,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tower::ServiceExt;

async fn probe(app: Router) -> (StatusCode, Value) {
  let req = Request::builder()
    .uri("/ready")
    .body(Body::empty())
    .unwrap();
  let res = app.oneshot(req).await.unwrap();
  let status = res.status();
  let bytes = res.into_body().collect().await.unwrap().to_bytes();
  (status, serde_json::from_slice(&bytes).unwrap())
}

fn up(name: &str) -> HealthIndicator {
  HealthIndicator::new(name, || succeed::<_, Infallible, _>(HealthOutcome::up()))
}

#[tokio::test]
async fn ready_reports_every_check_as_json_with_200_when_none_is_down() {
  let registry = HealthRegistry::new()
    .with_indicator(up("db"))
    .with_indicator(HealthIndicator::new("queue", || {
      succeed::<_, Infallible, _>(HealthOutcome::degraded(json!({ "lag": 7 })))
    }))
    .with_indicator(HealthIndicator::from_effect("disk", || {
      succeed::<_, Infallible, _>(json!({ "free_gb": 12 }))
    }));
  let (status, body) = probe(registry.routes()).await;
  assert_eq!(status, StatusCode::OK);
  assert_eq!(body["status"], "degraded");
  assert!(body.get("reason").is_none());
  assert_eq!(body["checks"]["db"]["status"], "up");
  assert_eq!(body["checks"]["db"]["detail"], Value::Null);
  assert!(body["checks"]["db"]["duration_ms"].is_u64());
  assert_eq!(body["checks"]["queue"]["detail"]["lag"], 7);
  assert_eq!(body["checks"]["disk"]["detail"]["free_gb"], 12);
}

#[tokio::test]
async fn a_failing_or_dying_check_is_down_and_ready_answers_503() {
  let registry = HealthRegistry::new()
    .with_indicator(up("ok"))
    .with_indicator(HealthIndicator::from_effect("db", || {
      fail::<(), _, _>("connection refused")
    }))
    .with_indicator(HealthIndicator::new("cache", || {
      die::<HealthOutcome, Infallible, _>("boom")
    }));
  let (status, body) = probe(registry.routes()).await;
  assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
  assert_eq!(body["status"], "down");
  assert_eq!(body["checks"]["ok"]["status"], "up");
  assert_eq!(body["checks"]["db"]["status"], "down");
  assert_eq!(
    body["checks"]["db"]["detail"]["error"],
    "connection refused"
  );
  assert_eq!(body["checks"]["cache"]["detail"]["error"], "boom");
}

#[tokio::test]
async fn a_timed_out_check_is_down_and_checks_run_concurrently() {
  let slow = |name: &str| {
    HealthIndicator::new(name, || {
      sleep::<Infallible, _>(Duration::from_millis(300)).map(|()| HealthOutcome::up())
    })
  };
  let registry = HealthRegistry::new()
    .with_indicator(slow("a"))
    .with_indicator(slow("b"))
    .with_indicator(slow("c"))
    .with_indicator(
      HealthIndicator::new("hung", || {
        sleep::<Infallible, _>(Duration::from_secs(3600)).map(|()| HealthOutcome::up())
      })
      .with_timeout(Duration::from_millis(100)),
    );
  let started = Instant::now();
  let (status, body) = probe(registry.routes()).await;
  let elapsed = started.elapsed();
  assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
  assert_eq!(body["checks"]["hung"]["status"], "down");
  assert_eq!(body["checks"]["hung"]["detail"]["error"], "timed out");
  assert_eq!(body["checks"]["hung"]["detail"]["timeout_ms"], 100);
  for name in ["a", "b", "c"] {
    assert_eq!(body["checks"][name]["status"], "up", "{name}");
  }
  // Three 300 ms checks in well under 900 ms: they ran concurrently.
  assert!(elapsed < Duration::from_millis(800), "{elapsed:?}");
}

#[tokio::test]
async fn a_readiness_check_adapts_into_an_indicator() {
  let flag = Arc::new(AtomicBool::new(false));
  let seen = Arc::clone(&flag);
  let check: ReadinessCheck = Arc::new(move || seen.load(Ordering::SeqCst));
  let app = HealthRegistry::new()
    .with_indicator(HealthIndicator::from_readiness("legacy", check))
    .routes();
  let (status, body) = probe(app.clone()).await;
  assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
  assert_eq!(body["checks"]["legacy"]["status"], "down");
  flag.store(true, Ordering::SeqCst);
  assert_eq!(probe(app).await.0, StatusCode::OK);
}

#[tokio::test]
async fn the_cache_indicator_reports_its_stats() {
  let cache: Cache<u32, u32> = Cache::new(8, None, |k: u32| succeed(k * 2));
  let warm: Effect<u32> = cache.get(1).flat_map({
    let cache = cache.clone();
    move |_| cache.get(1)
  });
  let warmed = id_effect_axum::EffectExecutor::global()
    .run_effect(warm)
    .await;
  assert!(matches!(warmed, id_effect::Exit::Success(2)), "{warmed:?}");
  let app = HealthRegistry::new()
    .with_indicator(HealthIndicator::cache("lookups", cache))
    .routes();
  let (status, body) = probe(app).await;
  assert_eq!(status, StatusCode::OK);
  let detail = &body["checks"]["lookups"]["detail"];
  assert_eq!(
    (&detail["hits"], &detail["misses"], &detail["size"]),
    (&json!(1), &json!(1), &json!(1))
  );
}

#[tokio::test]
async fn a_not_ready_state_forces_503_with_its_reason() {
  let state = ReadinessState::new(false);
  let app = HealthRegistry::new()
    .with_indicator(up("db"))
    .with_readiness(state.clone())
    .routes();
  let (status, body) = probe(app.clone()).await;
  assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
  assert_eq!(body["reason"], "not_ready");
  state.set_ready(true);
  assert_eq!(probe(app.clone()).await.0, StatusCode::OK);
  state.mark_shutting_down();
  let (status, body) = probe(app).await;
  assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
  assert_eq!(body["reason"], "shutting_down");
  assert_eq!(body["checks"]["db"]["status"], "up");
}

#[test]
fn status_severity_orders_up_degraded_down() {
  assert_eq!(
    HealthStatus::Up.worst(HealthStatus::Degraded),
    HealthStatus::Degraded
  );
  assert_eq!(
    HealthStatus::Down.worst(HealthStatus::Degraded),
    HealthStatus::Down
  );
}

async fn get_raw(addr: SocketAddr, path: &str) -> std::io::Result<String> {
  let mut stream = tokio::net::TcpStream::connect(addr).await?;
  stream
    .write_all(format!("GET {path} HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n").as_bytes())
    .await?;
  let mut out = String::new();
  stream.read_to_string(&mut out).await?;
  Ok(out)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_handle_shutdown_marks_readiness_shutting_down() {
  let readiness = ReadinessState::new(true);
  let config = HostConfig {
    bind_host: "127.0.0.1".into(),
    bind_port: 0,
    shutdown_timeout: Duration::from_secs(5),
  };
  let app = HealthRegistry::new()
    .with_readiness(readiness.clone())
    .routes();
  let handle = ServerHandle::start_with_readiness(&config, app, readiness.clone())
    .await
    .unwrap();
  let ok = get_raw(handle.local_addr(), "/ready").await.unwrap();
  assert!(ok.starts_with("HTTP/1.1 200"), "{ok}");
  handle.shutdown().await;
  assert!(readiness.is_shutting_down());
  assert_eq!(readiness.not_ready_reason(), Some("shutting_down"));
}

/// SIGTERM → `run_until_shutdown` and `serve_router` mark the host not ready first; while a
/// request is still in flight the listener keeps accepting and `/ready` answers 503.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn on_sigterm_ready_turns_503_while_an_in_flight_request_completes() {
  use id_effect_axum::{HostBuilder, bootstrap_env, serve_router};
  use id_effect_config::MapConfigProvider;
  use tokio::sync::{Notify, oneshot};

  let port = {
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    probe.local_addr().unwrap().port()
  };
  let addr: SocketAddr = ([127, 0, 0, 1], port).into();
  let port_text = port.to_string();
  let env = bootstrap_env(MapConfigProvider::from_pairs([
    ("HOST", "127.0.0.1"),
    ("PORT", port_text.as_str()),
    ("SHUTDOWN_TIMEOUT_SECS", "10"),
  ]));

  let started = Arc::new(Notify::new());
  let release = Arc::new(Notify::new());
  let (host_tx, host_rx) = oneshot::channel::<ReadinessState>();
  let (started2, release2) = (Arc::clone(&started), Arc::clone(&release));
  let server = tokio::spawn(
    HostBuilder::new()
      .with_env(env)
      .run_until_shutdown(move |host| {
        let _ = host_tx.send(host.readiness.clone());
        let drain = Arc::clone(&host.drain);
        let app = HealthRegistry::new()
          .with_indicator(up("db"))
          .with_readiness(host.readiness.clone())
          .routes()
          .route(
            "/slow",
            get(move || {
              let (drain, started, release) = (Arc::clone(&drain), started2, release2);
              async move {
                drain.enter();
                started.notify_one();
                release.notified().await;
                drain.leave();
                "done"
              }
            }),
          );
        serve_router(host, app)
      }),
  );
  let readiness = host_rx.await.unwrap();

  // Serving and ready (retry until the listener is bound).
  let deadline = Instant::now() + Duration::from_secs(10);
  loop {
    match get_raw(addr, "/ready").await {
      Ok(res) if res.starts_with("HTTP/1.1 200") => break,
      _ if Instant::now() < deadline => tokio::time::sleep(Duration::from_millis(20)).await,
      other => panic!("server never became ready: {other:?}"),
    }
  }

  let in_flight = tokio::spawn(get_raw(addr, "/slow"));
  started.notified().await;

  let killed = std::process::Command::new("kill")
    .args(["-TERM", &std::process::id().to_string()])
    .status()
    .unwrap();
  assert!(killed.success());

  // Not ready, reported over a fresh connection while /slow is still in flight.
  let deadline = Instant::now() + Duration::from_secs(10);
  let not_ready = loop {
    let res = get_raw(addr, "/ready").await.unwrap();
    if res.starts_with("HTTP/1.1 503") {
      break res;
    }
    assert!(Instant::now() < deadline, "never turned 503: {res}");
    tokio::time::sleep(Duration::from_millis(20)).await;
  };
  assert!(
    not_ready.contains("\"reason\":\"shutting_down\""),
    "{not_ready}"
  );
  assert!(readiness.is_shutting_down());
  assert!(!in_flight.is_finished());

  release.notify_one();
  let done = tokio::time::timeout(Duration::from_secs(10), in_flight)
    .await
    .unwrap()
    .unwrap()
    .unwrap();
  assert!(done.starts_with("HTTP/1.1 200"), "{done}");
  assert!(done.ends_with("done"), "{done}");
  tokio::time::timeout(Duration::from_secs(10), server)
    .await
    .unwrap()
    .unwrap()
    .unwrap();
}
