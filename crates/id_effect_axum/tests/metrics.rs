//! The Prometheus scrape endpoint, the HTTP metrics layer and the runtime fiber gauge.

use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::response::Response;
use http_body_util::BodyExt;
use id_effect::observability::{prometheus, runtime_metrics};
use id_effect::{Counter, HNil, Metric, MetricRegistry, Never, Runtime, sleep, sync};
use id_effect_axum::metrics::{
  HttpMetricsLayer, metrics_route_for, metrics_route_with, metrics_router,
};
use tokio::sync::Mutex;
use tower::ServiceExt;

/// Held by every test that starts fibers, so the live-fiber gauge sees only its own.
static FIBERS: Mutex<()> = Mutex::const_new(());

async fn get(app: &Router, uri: &str) -> Response {
  app
    .clone()
    .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
    .await
    .unwrap()
}

async fn text(res: Response) -> String {
  String::from_utf8(res.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap()
}

#[tokio::test]
async fn metrics_router_serves_the_global_registry_as_text_exposition_0_0_4() {
  Counter::new("metrics_test_scrapes").update_now(2);
  let app: Router = metrics_router();
  let res = get(&app, "/metrics").await;
  assert_eq!(res.status(), StatusCode::OK);
  assert_eq!(
    res.headers()[header::CONTENT_TYPE],
    "text/plain; version=0.0.4; charset=utf-8"
  );
  assert_eq!(
    res.headers()[header::CONTENT_TYPE],
    prometheus::CONTENT_TYPE
  );
  let body = text(res).await;
  assert!(
    body.contains("# TYPE metrics_test_scrapes_total counter\n"),
    "{body}"
  );
  assert!(body.contains("\nmetrics_test_scrapes_total 2\n"), "{body}");
  assert!(
    body.contains("# TYPE id_effect_fibers_live gauge\n"),
    "{body}"
  );
  assert!(
    body.contains("# TYPE id_effect_fibers_started_total counter\n"),
    "{body}"
  );
  assert!(
    body.contains("# TYPE id_effect_executor_in_flight gauge\n"),
    "{body}"
  );
}

#[tokio::test]
async fn metrics_route_for_renders_an_isolated_registry_after_on_scrape() {
  let registry = MetricRegistry::new();
  let scrapes = Arc::new(AtomicUsize::new(0));
  let gauge = registry.gauge("pool_idle");
  let app: Router = Router::new()
    .route("/m", metrics_route_for(registry.clone()))
    .route(
      "/sampled",
      metrics_route_with(registry, {
        let scrapes = Arc::clone(&scrapes);
        move || {
          let n = scrapes.fetch_add(1, Ordering::SeqCst) + 1;
          gauge.update_now(i64::try_from(n).unwrap());
        }
      }),
    );
  let body = text(get(&app, "/m").await).await;
  assert!(!body.contains("pool_idle"), "{body}");
  assert!(!body.contains("metrics_test_scrapes"), "{body}");
  get(&app, "/sampled").await;
  let body = text(get(&app, "/sampled").await).await;
  assert!(
    body.contains("# TYPE pool_idle gauge\npool_idle 2\n"),
    "{body}"
  );
}

#[tokio::test]
async fn http_metrics_layer_records_by_route_template_and_never_by_raw_path() {
  let registry = MetricRegistry::new();
  let app: Router = Router::new()
    .route("/users/{id}", axum::routing::get(|| async { "user" }))
    .route(
      "/fail",
      axum::routing::post(|| async { (StatusCode::CONFLICT, "no") }),
    )
    .layer(HttpMetricsLayer::new(registry.clone()));
  assert_eq!(get(&app, "/users/1").await.status(), StatusCode::OK);
  assert_eq!(get(&app, "/users/2").await.status(), StatusCode::OK);
  let post = Request::post("/fail").body(Body::empty()).unwrap();
  assert_eq!(
    app.clone().oneshot(post).await.unwrap().status(),
    StatusCode::CONFLICT
  );
  for path in ["/nope", "/scan/a", "/scan/b"] {
    assert_eq!(get(&app, path).await.status(), StatusCode::NOT_FOUND);
  }

  let body = prometheus::render(&registry);
  for line in [
    "# TYPE http_server_requests_total counter\n",
    "http_server_requests_total{method=\"GET\",route=\"/users/{id}\",status=\"200\"} 2\n",
    "http_server_requests_total{method=\"POST\",route=\"/fail\",status=\"409\"} 1\n",
    "http_server_requests_total{method=\"GET\",route=\"unmatched\",status=\"404\"} 3\n",
    "# TYPE http_server_request_duration_seconds histogram\n",
    "http_server_request_duration_seconds_count{method=\"GET\",route=\"/users/{id}\"} 2\n",
    "http_server_request_duration_seconds_bucket{method=\"GET\",route=\"/users/{id}\",le=\"+Inf\"} 2\n",
    "http_server_request_duration_seconds_count{method=\"GET\",route=\"unmatched\"} 3\n",
  ] {
    assert!(body.contains(line), "missing {line:?} in\n{body}");
  }
  for raw in ["/users/1", "/users/2", "/nope", "/scan"] {
    assert!(
      !body.contains(raw),
      "raw path {raw} became a series:\n{body}"
    );
  }
  let request_series = registry
    .snapshot()
    .into_iter()
    .filter(|p| p.key.name() == "http_server_requests_total")
    .count();
  assert_eq!(request_series, 3, "{body}");
}

#[tokio::test]
async fn effect_handlers_are_counted_and_fibers_return_to_baseline() {
  let _serial = FIBERS.lock().await;
  let baseline = runtime_metrics::fibers_live();
  let registry = MetricRegistry::new();
  let app: Router = Router::new()
    .route(
      "/effect",
      id_effect_axum::routing::get(|_: &mut ()| sync::<_, Infallible, ()>(|| "done")),
    )
    .layer(HttpMetricsLayer::new(registry.clone()));
  assert_eq!(get(&app, "/effect").await.status(), StatusCode::OK);
  assert!(
    prometheus::render(&registry)
      .contains("http_server_requests_total{method=\"GET\",route=\"/effect\",status=\"200\"} 1\n")
  );
  wait_for(
    || runtime_metrics::fibers_live() == baseline,
    "request fiber never finished",
  )
  .await;
}

#[tokio::test]
async fn forked_blocked_fibers_are_live_until_they_finish() {
  const N: usize = 16;
  let _serial = FIBERS.lock().await;
  let baseline = runtime_metrics::fibers_live();
  let started = runtime_metrics::fibers_started();
  let running = Arc::new(AtomicUsize::new(0));
  let handles: Vec<_> = (0..N)
    .map(|_| {
      let running = Arc::clone(&running);
      Runtime::global().fork(
        sync::<_, Never, HNil>(move || {
          running.fetch_add(1, Ordering::SeqCst);
        })
        .then(sleep(Duration::from_secs(3600))),
      )
    })
    .collect();
  wait_for(
    || running.load(Ordering::SeqCst) == N,
    "forked fibers never ran",
  )
  .await;
  let live = runtime_metrics::fibers_live();
  assert!(
    live >= baseline + N as u64,
    "live {live}, baseline {baseline}"
  );
  assert!(runtime_metrics::fibers_started() >= started + N as u64);
  let app: Router = metrics_router();
  let body = text(get(&app, "/metrics").await).await;
  let gauge: u64 = body
    .lines()
    .find_map(|l| l.strip_prefix("id_effect_fibers_live "))
    .and_then(|v| v.parse().ok())
    .unwrap_or_else(|| panic!("no fiber gauge in\n{body}"));
  assert!(gauge >= baseline + N as u64, "scraped {gauge}");

  for h in &handles {
    h.interrupt_now();
  }
  for h in handles {
    let _ = h.join_future().await;
  }
  // Each fiber is uncounted once (by `mark_done`, before its exit is published): never below.
  assert_eq!(runtime_metrics::fibers_live(), baseline);
}

async fn wait_for(mut cond: impl FnMut() -> bool, what: &str) {
  let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
  while !cond() {
    assert!(tokio::time::Instant::now() < deadline, "{what}");
    tokio::time::sleep(Duration::from_millis(1)).await;
  }
}
