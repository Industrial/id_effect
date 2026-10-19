//! Load shedding (M5) and the sharded LRU rate-limit store (M8) of the 0.11 security review.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::routing::get;
use http_body_util::BodyExt;
use id_effect::{Effect, Exit, HCons, HNil, Layer, from_async, hlist, need};
use id_effect_axum::middleware::{LoadShed, LoadShedLayer, MemoryRateLimitStore, RateLimitStore};
use id_effect_axum::{
  HostConfig, HostError, ServerHandle, bootstrap_env, load_host_config, server_layer,
};
use id_effect_config::MapConfigProvider;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Notify;
use tower::ServiceExt;

/// A `/slow` route that holds every request until `release` is notified, counting arrivals.
fn slow_app(entered: Arc<AtomicUsize>, release: Arc<Notify>) -> Router {
  Router::new()
    .route(
      "/slow",
      get(move || {
        let (entered, release) = (Arc::clone(&entered), Arc::clone(&release));
        async move {
          entered.fetch_add(1, Ordering::SeqCst);
          release.notified().await;
          "done"
        }
      }),
    )
    .route("/fast", get(|| async { "fast" }))
}

async fn wait_for(entered: &AtomicUsize, n: usize) {
  for _ in 0..500 {
    if entered.load(Ordering::SeqCst) >= n {
      return;
    }
    tokio::time::sleep(Duration::from_millis(2)).await;
  }
  panic!("handler never entered");
}

fn get_req(path: &str) -> Request<Body> {
  Request::builder().uri(path).body(Body::empty()).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn m5_load_shed_rejects_over_capacity_with_503() {
  let entered = Arc::new(AtomicUsize::new(0));
  let release = Arc::new(Notify::new());
  let shed = LoadShedLayer::new(1).retry_after(Duration::from_secs(3));
  let app = slow_app(Arc::clone(&entered), Arc::clone(&release)).layer(shed.clone());

  let first = tokio::spawn(app.clone().oneshot(get_req("/slow")));
  wait_for(&entered, 1).await;
  assert_eq!(shed.in_flight(), 1);

  // Full: answered at once (no queueing), whatever the route.
  let rejected = tokio::time::timeout(
    Duration::from_secs(1),
    app.clone().oneshot(get_req("/fast")),
  )
  .await
  .expect("a shed request is not queued")
  .unwrap();
  assert_eq!(rejected.status(), StatusCode::SERVICE_UNAVAILABLE);
  assert_eq!(rejected.headers()[header::RETRY_AFTER], "3");
  assert_eq!(
    rejected.headers()[header::CONTENT_TYPE],
    id_effect_axum::PROBLEM_JSON
  );
  let body = rejected.into_body().collect().await.unwrap().to_bytes();
  let problem: serde_json::Value = serde_json::from_slice(&body).unwrap();
  assert_eq!(problem["status"], 503);
  assert_eq!(problem["instance"], "/fast");

  // The slot frees when the admitted request is answered.
  release.notify_one();
  assert_eq!(first.await.unwrap().unwrap().status(), StatusCode::OK);
  assert_eq!(shed.in_flight(), 0);
  let ok = app.clone().oneshot(get_req("/fast")).await.unwrap();
  assert_eq!(ok.status(), StatusCode::OK);

  // A dropped request (client gone) frees its slot too.
  let dropped = tokio::spawn(app.clone().oneshot(get_req("/slow")));
  wait_for(&entered, 2).await;
  dropped.abort();
  let _ = dropped.await;
  assert_eq!(shed.in_flight(), 0);
}

async fn raw_get(addr: SocketAddr, path: &str) -> String {
  let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
  stream
    .write_all(format!("GET {path} HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n").as_bytes())
    .await
    .unwrap();
  let mut out = String::new();
  stream.read_to_string(&mut out).await.unwrap();
  out
}

fn local(load_shed: LoadShed) -> Layer<HCons<HostConfig, HNil>, HostError> {
  Layer::succeed(hlist![HostConfig {
    bind_host: "127.0.0.1".into(),
    bind_port: 0,
    shutdown_timeout: Duration::from_secs(5),
    load_shed,
  }])
}

/// Through `server_layer`: one slow request in flight, then a second one; the statuses.
async fn serve_two(load_shed: LoadShed) -> (String, String) {
  let entered = Arc::new(AtomicUsize::new(0));
  let release = Arc::new(Notify::new());
  let app = slow_app(Arc::clone(&entered), Arc::clone(&release));
  let probe: Effect<(String, String), HostError, HCons<ServerHandle, HNil>> =
    need::<ServerHandle>().flat_map(move |handle| {
      let addr = handle.local_addr();
      from_async(move || async move {
        let first = tokio::spawn(async move { raw_get(addr, "/slow").await });
        wait_for(&entered, 1).await;
        let second = tokio::spawn(async move { raw_get(addr, "/fast").await });
        let second = tokio::time::timeout(Duration::from_secs(2), second).await;
        release.notify_waiters();
        let first = first.await.unwrap();
        Ok((first, second.expect("answered without queueing").unwrap()))
      })
    });
  let program = probe.provide_layer(local(load_shed).provide_to(server_layer(app)));
  match id_effect_axum::EffectExecutor::global()
    .run_effect(program)
    .await
  {
    Exit::Success(pair) => pair,
    Exit::Failure(cause) => panic!("server probe failed: {cause:?}"),
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn m5_default_server_stack_has_concurrency_cap() {
  // The config every entry point loads caps in-flight requests with no setting at all.
  let env = bootstrap_env(MapConfigProvider::from_pairs(std::iter::empty::<(
    &str,
    &str,
  )>()));
  let loaded = id_effect_axum::EffectExecutor::global()
    .run_effect(load_host_config().provide_all(env))
    .await;
  let Exit::Success(config) = loaded else {
    panic!("default host config: {loaded:?}");
  };
  assert_eq!(
    config.load_shed.max_in_flight(),
    Some(LoadShed::default_max_in_flight())
  );
  assert_eq!(config, HostConfig::default());

  // `server_layer` installs it: with a cap of one, the second request is shed.
  let (first, second) = serve_two(LoadShed::new(1)).await;
  assert!(first.starts_with("HTTP/1.1 200"), "{first}");
  assert!(second.starts_with("HTTP/1.1 503"), "{second}");
  assert!(
    second.to_ascii_lowercase().contains("retry-after: 1"),
    "{second}"
  );

  // Only the recorded opt-out turns it off.
  let off = HostConfig::default().dangerous_no_load_shedding();
  assert_eq!(off.load_shed.max_in_flight(), None);
  let (first, second) = serve_two(off.load_shed).await;
  assert!(first.starts_with("HTTP/1.1 200"), "{first}");
  assert!(second.starts_with("HTTP/1.1 200"), "{second}");
  assert!(
    id_effect::observability::posture::dangerous_opt_outs()
      .iter()
      .any(|(id, _)| *id == "id_effect_axum::HostConfig::dangerous_no_load_shedding")
  );
}

fn admitted(store: &MemoryRateLimitStore, key: &str) -> bool {
  matches!(store.acquire(key).block_on(), Exit::Success(()))
}

#[test]
fn m8_eviction_evicts_least_recently_used() {
  // No refill and a burst of one: a held bucket stays empty, an evicted one comes back full.
  let store = MemoryRateLimitStore::new(0.0, 1.0).shards(1).max_keys(2);
  assert!(admitted(&store, "a"));
  assert!(admitted(&store, "b"));
  // `a` is used again (still empty), so `b` is now the least recently used.
  assert!(!admitted(&store, "a"));
  assert!(admitted(&store, "c"));
  assert_eq!(store.len(), 2);
  // `a` survived the eviction (still empty); `b` did not (a fresh bucket).
  assert!(!admitted(&store, "a"));
  assert!(admitted(&store, "b"));
  // That evicted `c`, the least recently used; `a` is still held.
  assert!(!admitted(&store, "a"));
  assert!(admitted(&store, "c"));
}

#[test]
fn m8_sharded_store_under_contention() {
  const THREADS: usize = 8;
  const PER_THREAD: usize = 2_000;
  let store = Arc::new(
    MemoryRateLimitStore::new(0.0, 100.0)
      .shards(16)
      .max_keys(1_024),
  );
  let admitted_hot = Arc::new(AtomicUsize::new(0));
  let workers: Vec<_> = (0..THREADS)
    .map(|t| {
      let (store, admitted_hot) = (Arc::clone(&store), Arc::clone(&admitted_hot));
      std::thread::spawn(move || {
        for i in 0..PER_THREAD {
          // A churn of distinct keys that keeps the shards evicting...
          let _ = admitted(&store, &format!("t{t}-k{i}"));
          // ...and one hot key every thread shares, used often enough to stay resident.
          if admitted(&store, "hot") {
            admitted_hot.fetch_add(1, Ordering::Relaxed);
          }
        }
      })
    })
    .collect();
  for worker in workers {
    worker.join().unwrap();
  }
  assert!(store.len() <= 1_024, "{} keys", store.len());
  // The hot bucket was shared and never lost: exactly its burst was admitted.
  assert_eq!(admitted_hot.load(Ordering::Relaxed), 100);
}
