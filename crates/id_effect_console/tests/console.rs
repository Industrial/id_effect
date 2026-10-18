//! The console over a real socket: the dashboard page at `/_console` and every API route, each
//! fetched anonymously, as a principal the guard denies, and as an operator.

use std::net::SocketAddr;

use axum::Router;
use axum::extract::Request;
use axum::http::header;
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::get;
use id_effect_authz::{BasicPrincipal, CurrentPrincipal, Guard, require_role};
use id_effect_console::Console;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const OPERATOR: &str = "operator-token";
const DEVELOPER: &str = "developer-token";

/// The app's authentication layer: a bearer token names the principal.
async fn authenticate(mut request: Request, next: Next) -> Response {
  let token = request
    .headers()
    .get(header::AUTHORIZATION)
    .and_then(|v| v.to_str().ok())
    .and_then(|v| v.strip_prefix("Bearer "))
    .map(str::to_owned);
  let principal = match token.as_deref() {
    Some(OPERATOR) => Some(BasicPrincipal::new("ops").with_role("operator")),
    Some(DEVELOPER) => Some(BasicPrincipal::new("dev").with_role("developer")),
    _ => None,
  };
  if let Some(principal) = principal {
    request
      .extensions_mut()
      .insert(CurrentPrincipal::new(principal));
  }
  next.run(request).await
}

/// Serves `console` merged into an app with one route of its own, on an ephemeral port.
async fn serve(console: Console) -> SocketAddr {
  let app = Router::new()
    .route("/hello", get(|| async { "hello" }))
    .merge(console.router(Guard::new("console.operate", require_role("operator"))))
    .layer(middleware::from_fn(authenticate));
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let addr = listener.local_addr().unwrap();
  tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
  addr
}

struct Reply {
  status: u16,
  headers: Vec<(String, String)>,
  body: String,
}

impl Reply {
  fn header(&self, name: &str) -> Option<&str> {
    self
      .headers
      .iter()
      .find(|(n, _)| n.eq_ignore_ascii_case(name))
      .map(|(_, v)| v.as_str())
  }

  fn json(&self) -> Value {
    serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("{e}: {}", self.body))
  }
}

/// One HTTP/1.1 request on a fresh connection; a body is sent as JSON.
async fn send(
  addr: SocketAddr,
  method: &str,
  path: &str,
  token: Option<&str>,
  body: Option<&str>,
) -> Reply {
  let content_type = body
    .is_some_and(|b| !b.is_empty())
    .then_some("application/json");
  send_as(addr, method, path, token, body, content_type).await
}

/// One HTTP/1.1 request on a fresh connection, with `content_type` (if any).
async fn send_as(
  addr: SocketAddr,
  method: &str,
  path: &str,
  token: Option<&str>,
  body: Option<&str>,
  content_type: Option<&str>,
) -> Reply {
  let mut request = format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
  if let Some(token) = token {
    request.push_str(&format!("Authorization: Bearer {token}\r\n"));
  }
  let body = body.unwrap_or_default();
  if let Some(content_type) = content_type {
    request.push_str(&format!("Content-Type: {content_type}\r\n"));
  }
  request.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
  let mut stream = TcpStream::connect(addr).await.unwrap();
  stream.write_all(request.as_bytes()).await.unwrap();
  let mut raw = Vec::new();
  stream.read_to_end(&mut raw).await.unwrap();
  let raw = String::from_utf8(raw).unwrap();
  let (head, body) = raw.split_once("\r\n\r\n").expect("an HTTP response");
  let mut lines = head.lines();
  let status = lines
    .next()
    .unwrap()
    .split(' ')
    .nth(1)
    .unwrap()
    .parse()
    .unwrap();
  let headers: Vec<(String, String)> = lines
    .filter_map(|l| l.split_once(':'))
    .map(|(n, v)| (n.trim().to_owned(), v.trim().to_owned()))
    .collect();
  assert!(
    !headers
      .iter()
      .any(|(n, v)| n.eq_ignore_ascii_case("transfer-encoding") && v.contains("chunked")),
    "{method} {path}: chunked answers are not parsed here"
  );
  Reply {
    status,
    headers,
    body: body.to_owned(),
  }
}

/// Every console route: `(method, path, body, operator's status)`.
fn routes() -> Vec<(&'static str, &'static str, Option<&'static str>, u16)> {
  let mut routes = vec![
    ("GET", "/_console", None, 200),
    ("GET", "/_console/api/fibers", None, 200),
    ("GET", "/_console/api/supervisors", None, 200),
    ("GET", "/_console/api/metrics", None, 200),
    ("GET", "/_console/api/health", None, 200),
    ("GET", "/_console/api/log-level", None, 200),
    (
      "PUT",
      "/_console/api/log-level",
      Some(r#"{"level":"info"}"#),
      200,
    ),
    ("GET", "/_console/api/no-such-route", None, 404),
  ];
  // Without a registered queue, every jobs route is a 404 behind the guard.
  if cfg!(feature = "jobs") {
    routes.push(("GET", "/_console/api/jobs", None, 404));
    routes.push(("POST", "/_console/api/jobs/1/retry", Some("{}"), 404));
    routes.push(("POST", "/_console/api/jobs/1/cancel", Some("{}"), 404));
  }
  routes
}

#[tokio::test(flavor = "multi_thread")]
async fn every_console_route_is_guarded_over_a_socket() {
  let addr = serve(Console::new()).await;
  for (method, path, body, ok) in routes() {
    let anonymous = send(addr, method, path, None, body).await;
    assert_eq!(anonymous.status, 401, "anonymous {method} {path}");
    assert_eq!(
      anonymous.header("content-type"),
      Some("application/problem+json"),
      "{method} {path}"
    );
    assert!(!anonymous.body.contains("<html"), "{method} {path}");

    let denied = send(addr, method, path, Some(DEVELOPER), body).await;
    assert_eq!(denied.status, 403, "developer {method} {path}");
    assert_eq!(denied.json()["policy"], "console.operate");

    let operator = send(addr, method, path, Some(OPERATOR), body).await;
    assert_eq!(operator.status, ok, "operator {method} {path}");
    assert_eq!(
      operator.header("cache-control"),
      Some("no-store"),
      "{method} {path}"
    );
    if path.contains("/api/") {
      let expected = if ok == 200 {
        "application/json"
      } else {
        "application/problem+json"
      };
      assert_eq!(operator.header("content-type"), Some(expected), "{path}");
      let _ = operator.json();
    }
  }
}

#[tokio::test(flavor = "multi_thread")]
async fn dashboard_is_one_self_contained_page() {
  let addr = serve(Console::new()).await;
  let page = send(addr, "GET", "/_console", Some(OPERATOR), None).await;
  assert_eq!(page.status, 200);
  assert_eq!(
    page.header("content-type"),
    Some("text/html; charset=utf-8")
  );
  assert!(page.body.contains("<title>id_effect console</title>"));
  for route in ["fibers", "supervisors", "metrics", "health", "log-level"] {
    assert!(page.body.contains(&format!("'{route}'")), "{route}");
  }
  let csp = page.header("content-security-policy").expect("a CSP");
  assert!(csp.contains("default-src 'none'"), "{csp}");
  assert!(csp.contains("connect-src 'self'"), "{csp}");
  assert_eq!(page.header("x-content-type-options"), Some("nosniff"));
  assert!(!page.body.contains("https://") && !page.body.contains("http://"));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_guard_stays_on_the_console() {
  let addr = serve(Console::new()).await;
  // The app's own routes and fallback are not behind the console guard.
  let hello = send(addr, "GET", "/hello", None, None).await;
  assert_eq!((hello.status, hello.body.as_str()), (200, "hello"));
  assert_eq!(
    send(addr, "GET", "/elsewhere", None, None).await.status,
    404
  );
  // The page answers only at `/_console`; a wrong method changes nothing.
  let post = send(addr, "POST", "/_console", Some(OPERATOR), None).await;
  assert_eq!(post.status, 405);
  assert!(post.body.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn log_level_form_target_changes_the_level() {
  let addr = serve(Console::new()).await;
  let body = Some(r#"{"level":"warn"}"#);
  let put = send(addr, "PUT", "/_console/api/log-level", Some(OPERATOR), body).await;
  assert_eq!(put.status, 200);
  assert_eq!(put.json()["logger"], "warn");
  let get = send(addr, "GET", "/_console/api/log-level", Some(OPERATOR), None).await;
  assert_eq!(get.json()["logger"], "warn");

  let bad = Some(r#"{"level":"loud"}"#);
  let rejected = send(addr, "PUT", "/_console/api/log-level", Some(OPERATOR), bad).await;
  assert_eq!(rejected.status, 400);
  assert_eq!(
    rejected.header("content-type"),
    Some("application/problem+json")
  );
}

#[cfg(feature = "jobs")]
mod jobs {
  use std::time::Duration;

  use id_effect::{Effect, Exit, WallClock};
  use id_effect_axum::EffectExecutor;
  use id_effect_console::{Console, JobView, JobsView};
  use id_effect_jobs::{
    JobError, JobFailure, JobId, JobQueue, JobStore, MemoryJobQueue, NewJob, Priority, Queue, RunAt,
  };
  use serde_json::json;

  use super::{DEVELOPER, OPERATOR, send, send_as, serve};

  async fn run<A: Send + 'static>(effect: Effect<A, JobError>) -> A {
    match EffectExecutor::global().run_effect(effect).await {
      Exit::Success(a) => a,
      Exit::Failure(cause) => panic!("queue operation failed: {cause:?}"),
    }
  }

  /// A queue with one job enqueued, and its id.
  async fn one_job(max_attempts: u16) -> (MemoryJobQueue, JobId) {
    let queue = MemoryJobQueue::new(WallClock::system());
    let id = run(queue.enqueue_raw(NewJob {
      max_attempts,
      ..job("app.mail")
    }))
    .await
    .id()
    .unwrap();
    (queue, id)
  }

  /// Claims job `id` and fails it for good: it is `dead`.
  async fn kill(queue: &MemoryJobQueue, id: JobId) {
    let lease = Duration::from_secs(60);
    let claimed = run(queue.claim(&Queue::default(), "w", 1, lease)).await;
    assert_eq!(claimed[0].id, id);
    run(queue.fail(id, "w", &JobFailure::discard("smtp refused"), None)).await;
  }

  fn post(action: &str, id: JobId) -> String {
    format!("/_console/api/jobs/{}/{action}", id.0)
  }

  fn job(name: &str) -> NewJob {
    NewJob {
      name: name.to_owned(),
      queue: Queue::default(),
      payload: json!({ "email": "ada@example.com" }),
      priority: Priority::NORMAL,
      max_attempts: 3,
      unique: None,
      run_at: RunAt::Now,
    }
  }

  #[tokio::test(flavor = "multi_thread")]
  async fn jobs_route_reports_counts_and_recent_jobs_without_payloads() {
    let queue = MemoryJobQueue::new(WallClock::system());
    for name in ["app.first", "app.second"] {
      let enqueued = EffectExecutor::global()
        .run_effect(queue.enqueue_raw(job(name)))
        .await;
      assert!(
        matches!(enqueued, Exit::Success(ref e) if e.is_new()),
        "{enqueued:?}"
      );
    }
    let addr = serve(Console::new().jobs(queue)).await;

    assert_eq!(
      send(addr, "GET", "/_console/api/jobs", Some(DEVELOPER), None)
        .await
        .status,
      403
    );
    let answer = send(addr, "GET", "/_console/api/jobs", Some(OPERATOR), None).await;
    assert_eq!(answer.status, 200);
    assert_eq!(answer.header("cache-control"), Some("no-store"));
    let raw = answer.json();
    assert!(raw["recent"][0].get("payload").is_none(), "{raw}");
    assert!(!answer.body.contains("ada@example.com"));

    let view: JobsView = serde_json::from_value(raw).unwrap();
    assert_eq!(view.queues.len(), 1);
    assert_eq!(view.queues[0].queue, "default");
    assert_eq!(view.queues[0].counts.get("scheduled"), Some(&2));
    assert!(view.queues[0].oldest_runnable.is_some());
    let names: Vec<&str> = view.recent.iter().map(|j| j.name.as_str()).collect();
    assert_eq!(names, ["app.second", "app.first"], "newest first");
    assert_eq!(view.recent[0].state, "scheduled");
    assert_eq!(view.recent[0].max_attempts, 3);
  }

  #[tokio::test(flavor = "multi_thread")]
  async fn a_queue_outage_is_a_500_problem() {
    let queue = MemoryJobQueue::new(WallClock::system());
    queue.fail_with(Some("queue database down"));
    let addr = serve(Console::new().jobs(queue)).await;

    let answer = send(addr, "GET", "/_console/api/jobs", Some(OPERATOR), None).await;
    assert_eq!(answer.status, 500);
    assert_eq!(
      answer.header("content-type"),
      Some("application/problem+json")
    );
    let detail = answer.json()["detail"].as_str().unwrap().to_owned();
    assert!(detail.contains("queue database down"), "{detail}");
  }

  #[tokio::test(flavor = "multi_thread")]
  async fn job_actions_are_guarded() {
    let (queue, id) = one_job(1).await;
    kill(&queue, id).await;
    let addr = serve(Console::new().jobs(queue.clone())).await;
    for action in ["retry", "cancel"] {
      let path = post(action, id);
      let anonymous = send(addr, "POST", &path, None, Some("{}")).await;
      assert_eq!(anonymous.status, 401, "{path}");
      let denied = send(addr, "POST", &path, Some(DEVELOPER), Some("{}")).await;
      assert_eq!(denied.status, 403, "{path}");
      assert_eq!(denied.json()["policy"], "console.operate");
    }
    let jobs = run(queue.list(Default::default())).await;
    assert_eq!(
      jobs[0].state.as_str(),
      "dead",
      "a denied retry changed nothing"
    );
  }

  #[tokio::test(flavor = "multi_thread")]
  async fn retry_makes_a_dead_job_runnable() {
    let (queue, id) = one_job(1).await;
    kill(&queue, id).await;
    let addr = serve(Console::new().jobs(queue.clone())).await;

    let answer = send(addr, "POST", &post("retry", id), Some(OPERATOR), Some("{}")).await;
    assert_eq!(answer.status, 200, "{}", answer.body);
    assert_eq!(answer.header("cache-control"), Some("no-store"));
    let view: JobView = serde_json::from_value(answer.json()).unwrap();
    assert_eq!(
      (view.id, view.state.as_str(), view.attempts),
      (id.0, "scheduled", 0)
    );
    assert_eq!(view.finished_at, None);
    // A worker can claim it again.
    let lease = Duration::from_secs(60);
    let claimed = run(queue.claim(&Queue::default(), "w2", 1, lease)).await;
    assert_eq!(claimed.len(), 1);
    assert_eq!((claimed[0].id, claimed[0].attempt), (id, 1));

    // Now running: neither action applies.
    for action in ["retry", "cancel"] {
      let conflict = send(addr, "POST", &post(action, id), Some(OPERATOR), Some("{}")).await;
      assert_eq!(conflict.status, 409, "{action}");
      assert_eq!(
        conflict.header("content-type"),
        Some("application/problem+json")
      );
      let detail = conflict.json()["detail"].as_str().unwrap().to_owned();
      assert!(detail.contains("is running"), "{detail}");
    }
  }

  #[tokio::test(flavor = "multi_thread")]
  async fn cancel_stops_a_scheduled_job_and_retry_brings_it_back() {
    let (queue, id) = one_job(3).await;
    let addr = serve(Console::new().jobs(queue.clone())).await;

    let early = send(addr, "POST", &post("retry", id), Some(OPERATOR), Some("{}")).await;
    assert_eq!(early.status, 409, "a scheduled job is not retried");

    let cancelled = send(
      addr,
      "POST",
      &post("cancel", id),
      Some(OPERATOR),
      Some("{}"),
    )
    .await;
    assert_eq!(cancelled.status, 200, "{}", cancelled.body);
    assert_eq!(cancelled.json()["state"], "cancelled");
    let lease = Duration::from_secs(60);
    assert!(
      run(queue.claim(&Queue::default(), "w", 1, lease))
        .await
        .is_empty(),
      "a cancelled job is not claimed"
    );
    let again = send(
      addr,
      "POST",
      &post("cancel", id),
      Some(OPERATOR),
      Some("{}"),
    )
    .await;
    assert_eq!(again.status, 409);

    let retried = send(addr, "POST", &post("retry", id), Some(OPERATOR), Some("{}")).await;
    assert_eq!(retried.status, 200);
    assert_eq!(retried.json()["state"], "scheduled");
  }

  #[tokio::test(flavor = "multi_thread")]
  async fn unknown_or_malformed_ids_are_problems() {
    let (queue, _) = one_job(3).await;
    let addr = serve(Console::new().jobs(queue)).await;
    for action in ["retry", "cancel"] {
      let missing = send(
        addr,
        "POST",
        &post(action, JobId(99)),
        Some(OPERATOR),
        Some("{}"),
      )
      .await;
      assert_eq!(missing.status, 404, "{action}");
      assert_eq!(missing.json()["detail"], "no job with id 99");
      let path = format!("/_console/api/jobs/abc/{action}");
      let malformed = send(addr, "POST", &path, Some(OPERATOR), Some("{}")).await;
      assert_eq!(malformed.status, 400, "{action}");
      assert_eq!(
        malformed.header("content-type"),
        Some("application/problem+json")
      );
    }
  }

  /// What a cross-site `<form method=post>` (or a `fetch` without a preflight) can send is
  /// refused before the queue is touched.
  #[tokio::test(flavor = "multi_thread")]
  async fn job_actions_refuse_what_a_cross_site_form_can_send() {
    let (queue, id) = one_job(3).await;
    let addr = serve(Console::new().jobs(queue.clone())).await;
    let path = post("cancel", id);
    for content_type in [
      None,
      Some("application/x-www-form-urlencoded"),
      Some("multipart/form-data; boundary=x"),
      Some("text/plain"),
    ] {
      let refused = send_as(
        addr,
        "POST",
        &path,
        Some(OPERATOR),
        Some("a=b"),
        content_type,
      )
      .await;
      assert_eq!(refused.status, 415, "{content_type:?}");
      assert_eq!(
        refused.header("content-type"),
        Some("application/problem+json")
      );
    }
    let jobs = run(queue.list(Default::default())).await;
    assert_eq!(jobs[0].state.as_str(), "scheduled", "nothing was cancelled");
    // The JSON content type with parameters is accepted, with or without a body.
    let json = Some("application/json; charset=utf-8");
    let accepted = send_as(addr, "POST", &path, Some(OPERATOR), None, json).await;
    assert_eq!(accepted.status, 200, "{}", accepted.body);
  }
}
