//! Cancellation (design note §2): a client disconnect and an expired deadline interrupt the
//! handler fiber, and its finalizers run; the client's deadline is capped by the server's.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use id_effect::{DynEnv, Never, Stream, sleep, succeed, sync};
use id_effect_rpc::{Procedure, RpcGroup, RpcServer, WireSchema};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tower::ServiceExt;

#[derive(Default)]
struct Flags {
  started: AtomicBool,
  finalized: AtomicBool,
}

impl Flags {
  async fn wait(&self, flag: fn(&Self) -> &AtomicBool, within: Duration) -> bool {
    let until = Instant::now() + within;
    while Instant::now() < until {
      if flag(self).load(Ordering::SeqCst) {
        return true;
      }
      tokio::time::sleep(Duration::from_millis(5)).await;
    }
    flag(self).load(Ordering::SeqCst)
  }
}

/// An endless source that records being pulled, and being released.
struct Ticks {
  n: i64,
  flags: Arc<Flags>,
}

impl Iterator for Ticks {
  type Item = i64;

  fn next(&mut self) -> Option<i64> {
    self.flags.started.store(true, Ordering::SeqCst);
    self.n += 1;
    Some(self.n)
  }
}

impl Drop for Ticks {
  fn drop(&mut self) {
    self.flags.finalized.store(true, Ordering::SeqCst);
  }
}

/// `Slow.wait` sleeps an hour inside an `ensuring` finalizer; `Slow.ticks` streams forever.
fn group(flags: &Arc<Flags>) -> RpcGroup {
  let mut group = RpcGroup::new();
  let f = flags.clone();
  group.query(
    Procedure::query("Slow.wait").public(),
    WireSchema::new(id_effect::schema::i64::<()>()),
    WireSchema::new(id_effect::schema::i64::<()>()),
    move |n: i64, _: &mut DynEnv| {
      let started = f.clone();
      let finalized = f.clone();
      sync::<(), Never, DynEnv>(move || started.started.store(true, Ordering::SeqCst))
        .flat_map(|()| sleep(Duration::from_secs(3600)))
        .map(move |()| n)
        .ensuring(sync(move || {
          finalized.finalized.store(true, Ordering::SeqCst)
        }))
    },
  );
  let f = flags.clone();
  group.server_stream(
    Procedure::server_stream("Slow.ticks").public(),
    WireSchema::new(id_effect::schema::i64::<()>()),
    WireSchema::new(id_effect::schema::i64::<()>()),
    move |_: i64, _: &mut DynEnv| {
      let ticks = Ticks {
        n: 0,
        flags: f.clone(),
      };
      succeed::<_, Never, _>(Stream::<i64, Never, DynEnv>::from_iterable(ticks))
    },
  );
  group
}

fn slow_request(deadline_ms: Option<u64>) -> Request<Body> {
  let mut req = Request::post("/rpc/Slow.wait")
    .header("content-type", "application/json")
    .header("x-rpc", "1");
  if let Some(ms) = deadline_ms {
    req = req.header("x-rpc-deadline-ms", ms.to_string());
  }
  req.body(Body::from("1")).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn client_disconnect_interrupts_handler_and_runs_finalizers() {
  let flags = Arc::new(Flags::default());
  let app: axum::Router = RpcServer::new(group(&flags))
    .with_max_deadline(Duration::from_secs(3600))
    .mount_default()
    .with_state(DynEnv::new());
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let addr = listener.local_addr().unwrap();
  let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

  let mut socket = tokio::net::TcpStream::connect(addr).await.unwrap();
  let request = "POST /rpc/Slow.wait HTTP/1.1\r\nhost: localhost\r\ncontent-type: application/json\r\nx-rpc: 1\r\ncontent-length: 1\r\n\r\n1";
  socket.write_all(request.as_bytes()).await.unwrap();
  assert!(
    flags.wait(|f| &f.started, Duration::from_secs(5)).await,
    "the handler started"
  );
  assert!(!flags.finalized.load(Ordering::SeqCst));
  // The client goes away mid-call.
  drop(socket);
  assert!(
    flags.wait(|f| &f.finalized, Duration::from_secs(5)).await,
    "the disconnect interrupted the handler fiber and its finalizer ran"
  );
  server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dropping_a_call_interrupts_it() {
  // The same rule without a socket: the service future of the call is dropped mid-call.
  let flags = Arc::new(Flags::default());
  let app: axum::Router = RpcServer::new(group(&flags))
    .with_max_deadline(Duration::from_secs(3600))
    .mount_default()
    .with_state(DynEnv::new());
  let call = tokio::spawn(app.oneshot(slow_request(None)));
  assert!(flags.wait(|f| &f.started, Duration::from_secs(5)).await);
  call.abort();
  assert!(flags.wait(|f| &f.finalized, Duration::from_secs(5)).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deadline_capped_by_server_max() {
  let flags = Arc::new(Flags::default());
  let app: axum::Router = RpcServer::new(group(&flags))
    .with_max_deadline(Duration::from_millis(100))
    .mount_default()
    .with_state(DynEnv::new());
  let begun = Instant::now();
  // The client asks for ten minutes; the server allows 100 ms.
  let res = app
    .clone()
    .oneshot(slow_request(Some(600_000)))
    .await
    .unwrap();
  let took = begun.elapsed();
  assert_eq!(res.status(), StatusCode::GATEWAY_TIMEOUT);
  assert!(took < Duration::from_secs(5), "took {took:?}");
  let body: Value =
    serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
  assert_eq!(
    body["exit"]["error"],
    serde_json::json!({"_tag": "DeadlineExceeded"})
  );
  assert!(
    flags.wait(|f| &f.finalized, Duration::from_secs(5)).await,
    "the expired call's finalizer ran"
  );

  // A shorter client deadline wins over a long server maximum.
  let flags = Arc::new(Flags::default());
  let app: axum::Router = RpcServer::new(group(&flags))
    .with_max_deadline(Duration::from_secs(3600))
    .mount_default()
    .with_state(DynEnv::new());
  let res = app.oneshot(slow_request(Some(20))).await.unwrap();
  assert_eq!(res.status(), StatusCode::GATEWAY_TIMEOUT);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn disconnecting_from_a_server_stream_stops_its_producer() {
  let flags = Arc::new(Flags::default());
  let app: axum::Router = RpcServer::new(group(&flags))
    .with_max_deadline(Duration::from_secs(3600))
    .with_stream_window(1)
    .mount_default()
    .with_state(DynEnv::new());
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let addr = listener.local_addr().unwrap();
  let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
  let mut socket = tokio::net::TcpStream::connect(addr).await.unwrap();
  let request = "POST /rpc/Slow.ticks HTTP/1.1\r\nhost: localhost\r\ncontent-type: application/json\r\nx-rpc: 1\r\ncontent-length: 1\r\n\r\n0";
  socket.write_all(request.as_bytes()).await.unwrap();
  let mut buf = vec![0u8; 4096];
  let n = socket.read(&mut buf).await.unwrap();
  let head = String::from_utf8_lossy(&buf[..n]);
  assert!(head.starts_with("HTTP/1.1 200"), "{head}");
  assert!(flags.started.load(Ordering::SeqCst));
  assert!(!flags.finalized.load(Ordering::SeqCst));
  drop(socket);
  assert!(
    flags.wait(|f| &f.finalized, Duration::from_secs(5)).await,
    "the disconnect dropped the call and released the endless producer"
  );
  server.abort();
}

#[cfg(feature = "test-util")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cancel_signal_interrupts_the_call() {
  use id_effect_rpc::cancel::{CallControl, cancel_pair};
  use id_effect_rpc::kinds::InProcess;
  use id_effect_rpc::wire::RpcFailure;

  let flags = Arc::new(Flags::default());
  let (handle, signal) = cancel_pair();
  let rpc = InProcess::new(group(&flags), DynEnv::new())
    .with_control(CallControl::none().with_cancel(signal));
  let call = tokio::spawn(async move { rpc.unary("Slow.wait", serde_json::json!(1)).await });
  assert!(flags.wait(|f| &f.started, Duration::from_secs(5)).await);
  handle.cancel();
  let outcome = call.await.unwrap().unwrap();
  assert_eq!(outcome.reserved(), Some(RpcFailure::Cancelled));
  assert!(flags.wait(|f| &f.finalized, Duration::from_secs(5)).await);
}
