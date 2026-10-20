//! Server-sent events (feature `sse`): resume from `Last-Event-ID`, heartbeats on the test
//! clock, stream caps and interruption when the client goes away.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::http::request::Parts;
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use http_body_util::BodyExt;
use id_effect::{Effect, Exit, Never, Stream, TestExecutor, die, fail, sleep, succeed, sync};
use id_effect_axum::RequestCaps;
use id_effect_axum::realtime::sse::{self, SseConfig, SseEvent, SseRequest};
use id_effect_axum::route::{GuardFuture, Problem, RouteGuard, Router};
use tower::ServiceExt;

/// Events `n..n + 5` with their sequence numbers as ids, resuming after `Last-Event-ID`.
fn ticker(_: &mut (), _: RequestCaps, req: SseRequest) -> Effect<Stream<SseEvent>, Problem, ()> {
  let from = match req.last_event_id {
    None => 0,
    Some(id) => match id.as_u64() {
      Some(n) => n + 1,
      None => return fail(Problem::new(StatusCode::BAD_REQUEST).with_detail("unknown event id")),
    },
  };
  succeed(Stream::from_iterable(from..from + 5).map(|n| {
    SseEvent::data(format!("tick {n}"))
      .id(n.to_string())
      .event("tick")
  }))
}

fn app(config: SseConfig) -> axum::Router {
  Router::new()
    .route(sse::endpoint("/ticks", config, ticker).public())
    .with_state(())
    .into_axum()
}

fn get(last_event_id: Option<&str>) -> Request<Body> {
  let mut req = Request::get("/ticks");
  if let Some(id) = last_event_id {
    req = req.header("last-event-id", id);
  }
  req.body(Body::empty()).unwrap()
}

async fn text(res: Response) -> String {
  let bytes = res.into_body().collect().await.unwrap().to_bytes();
  String::from_utf8(bytes.to_vec()).unwrap()
}

fn ids(body: &str) -> Vec<u64> {
  body
    .lines()
    .filter_map(|l| l.strip_prefix("id: "))
    .map(|id| id.parse().unwrap())
    .collect()
}

#[tokio::test]
async fn sse_resumes_from_last_event_id() {
  let app = app(SseConfig::default());
  let res = app.clone().oneshot(get(None)).await.unwrap();
  assert_eq!(res.status(), StatusCode::OK);
  assert_eq!(res.headers()["content-type"], "text/event-stream");
  assert_eq!(res.headers()["cache-control"], "no-cache");
  let body = text(res).await;
  assert_eq!(ids(&body), [0, 1, 2, 3, 4]);
  assert!(
    body.starts_with("event: tick\ndata: tick 0\nid: 0\n\n"),
    "{body}"
  );
  // The client reconnects with the last id it saw: the stream resumes right after it.
  let body = text(app.clone().oneshot(get(Some("3"))).await.unwrap()).await;
  assert_eq!(ids(&body), [4, 5, 6, 7, 8]);
  // An empty id resets.
  let body = text(app.clone().oneshot(get(Some(""))).await.unwrap()).await;
  assert_eq!(ids(&body)[0], 0);
  // The handler's typed failure answers its own response.
  let res = app.clone().oneshot(get(Some("x"))).await.unwrap();
  assert_eq!(res.status(), StatusCode::BAD_REQUEST);
  // A malformed header never reaches the handler.
  let long = "9".repeat(300);
  let res = app.oneshot(get(Some(&long))).await.unwrap();
  assert_eq!(res.status(), StatusCode::BAD_REQUEST);
  assert_eq!(res.headers()["content-type"], "application/problem+json");
}

/// Collects the chunks of `encode(events)` on a test executor, advancing `step` at a time.
#[test]
fn heartbeats_follow_the_test_clock() {
  let exec = TestExecutor::new();
  let events = Stream::<SseEvent>::from_effect(
    sleep(Duration::from_secs(40)).map(|()| vec![SseEvent::data("late").id("1")]),
  );
  let config = SseConfig::default()
    .keep_alive(Duration::from_secs(15))
    .retry(Duration::from_secs(3));
  let out: Arc<Mutex<Option<Vec<Bytes>>>> = Arc::new(Mutex::new(None));
  let slot = Arc::clone(&out);
  let run = sse::encode(events, &config)
    .run_collect()
    .map(move |chunks| {
      *slot.lock().unwrap() = Some(chunks);
    });
  drop(run.fork_root(Arc::new(exec.clone())));
  exec.run_until_stalled();
  for _ in 0..4 {
    exec.advance(Duration::from_secs(10));
    exec.run_until_stalled();
  }
  let chunks = out.lock().unwrap().take().expect("the stream ended");
  let chunks: Vec<&[u8]> = chunks.iter().map(|b| b.as_ref()).collect();
  assert_eq!(
    chunks,
    [
      &b"retry: 3000\n\n"[..],
      b":\n\n",
      b":\n\n",
      b"data: late\nid: 1\n\n"
    ]
  );
}

#[test]
fn a_failing_or_dying_event_stream_fails_the_bytes() {
  let exec = TestExecutor::new();
  let config = SseConfig::default();
  let failing = Stream::<SseEvent, String>::from_vec(vec![SseEvent::data("a")])
    .chain(Stream::fail("broken".to_string()));
  type Outcome = Arc<Mutex<Option<Exit<Vec<Bytes>, String>>>>;
  let exit: Outcome = Arc::new(Mutex::new(None));
  let slot = Arc::clone(&exit);
  drop(
    sse::encode(failing, &config)
      .run_collect()
      .exit::<Never>()
      .map(move |e| *slot.lock().unwrap() = Some(e))
      .fork_root(Arc::new(exec.clone())),
  );
  exec.run_until_stalled();
  match exit.lock().unwrap().take() {
    Some(Exit::Failure(cause)) => assert!(format!("{cause:?}").contains("broken")),
    other => panic!("expected the failure, got {other:?}"),
  }
  let dying = Stream::<SseEvent, String>::from_effect(die("kaboom"));
  let slot = Arc::clone(&exit);
  drop(
    sse::encode(dying, &config)
      .run_collect()
      .exit::<Never>()
      .map(move |e| *slot.lock().unwrap() = Some(e))
      .fork_root(Arc::new(exec.clone())),
  );
  exec.run_until_stalled();
  match exit.lock().unwrap().take() {
    Some(Exit::Failure(cause)) => assert!(format!("{cause:?}").contains("kaboom")),
    other => panic!("expected the defect, got {other:?}"),
  }
}

#[tokio::test]
async fn dropping_the_response_interrupts_the_producer() {
  let finalized = Arc::new(AtomicBool::new(false));
  let f = Arc::clone(&finalized);
  let events = Stream::<SseEvent, std::convert::Infallible>::from_vec(vec![SseEvent::data(
    "first",
  )])
  .chain(Stream::from_effect(
    sleep(Duration::from_secs(3600))
      .map(|()| Vec::new())
      .ensuring(sync(move || f.store(true, Ordering::SeqCst))),
  ));
  let res = sse::response(events, &SseConfig::default());
  let mut body = res.into_body();
  let first = body.frame().await.unwrap().unwrap().into_data().unwrap();
  assert_eq!(first, "data: first\n\n");
  drop(body);
  tokio::time::timeout(Duration::from_secs(10), async {
    while !finalized.load(Ordering::SeqCst) {
      tokio::time::sleep(Duration::from_millis(1)).await;
    }
  })
  .await
  .expect("the producer's finalizer runs once the client is gone");
}

fn forever(_: &mut (), _: RequestCaps, _: SseRequest) -> Effect<Stream<SseEvent>, Never, ()> {
  succeed(Stream::from_effect(
    sleep(Duration::from_secs(3600)).map(|()| Vec::new()),
  ))
}

#[tokio::test]
async fn open_streams_are_capped() {
  let app: axum::Router = Router::new()
    .route(sse::endpoint("/live", SseConfig::default().max_streams(1, 1), forever).public())
    .with_state(())
    .into_axum();
  let req = || Request::get("/live").body(Body::empty()).unwrap();
  let first = app.clone().oneshot(req()).await.unwrap();
  assert_eq!(first.status(), StatusCode::OK);
  let res = app.clone().oneshot(req()).await.unwrap();
  assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
  assert_eq!(res.headers()["retry-after"], "1");
  drop(first);
  let reopened = tokio::time::timeout(Duration::from_secs(10), async {
    loop {
      let res = app.clone().oneshot(req()).await.unwrap();
      if res.status() == StatusCode::OK {
        break res;
      }
      tokio::time::sleep(Duration::from_millis(1)).await;
    }
  })
  .await
  .expect("a closed stream frees its slot");
  assert_eq!(reopened.headers()["content-type"], "text/event-stream");
}

struct Deny;

impl RouteGuard for Deny {
  fn policy(&self) -> &str {
    "nobody"
  }

  fn check<'a>(&'a self, _: &'a Parts) -> GuardFuture<'a> {
    Box::pin(async { Err(StatusCode::UNAUTHORIZED.into_response()) })
  }
}

#[tokio::test]
async fn guarded_streams_authenticate_first() {
  let app: axum::Router = Router::new()
    .route(sse::endpoint("/ticks", SseConfig::default(), ticker).guard(Deny))
    .with_state(())
    .into_axum();
  let res = app.oneshot(get(None)).await.unwrap();
  assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}
