//! The console API end to end: the guard in front of every route, and each panel's JSON.

use std::io;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use id_effect::{Effect, Exit, HNil, Metric, MetricRegistry, Never, Runtime, sleep, succeed, sync};
use id_effect_authz::{BasicPrincipal, CurrentPrincipal, Guard, require_role};
use id_effect_axum::{EffectExecutor, HealthIndicator, HealthOutcome, HealthRegistry};
use id_effect_console::{
  API_PREFIX, Console, FiberView, LogLevelView, MetricView, SupervisorsView,
};
use id_effect_logger::LogLevel;
use id_effect_logger::{
  EffectLogger, EffectLoggerError, global_minimum_log_level, provide_minimum_log_level,
};
use id_effect_node::control::TreeNodeView;
use id_effect_process::{
  ChildSpec, Next, NodeName, Pid, Process, ProcessCtx, ProcessNode, ShutdownPolicy, StartFn,
  Strategy, SupervisorHandle, SupervisorSpec,
};
use serde_json::{Value, json};
use tower::ServiceExt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::{EnvFilter, Registry, reload};

const T: Duration = Duration::from_secs(5);

fn guard() -> Guard {
  Guard::new("console.operate", require_role("operator"))
}

fn operator() -> Option<CurrentPrincipal> {
  Some(CurrentPrincipal::new(
    BasicPrincipal::new("ops").with_role("operator"),
  ))
}

fn developer() -> Option<CurrentPrincipal> {
  Some(CurrentPrincipal::new(
    BasicPrincipal::new("dev").with_role("developer"),
  ))
}

struct Answer {
  status: StatusCode,
  content_type: String,
  cache_control: Option<String>,
  body: Value,
}

async fn call(
  app: &Router,
  method: Method,
  route: &str,
  principal: Option<CurrentPrincipal>,
  body: Option<Value>,
) -> Answer {
  let mut request = Request::builder()
    .method(method)
    .uri(format!("{API_PREFIX}/{route}"));
  if body.is_some() {
    request = request.header(header::CONTENT_TYPE, "application/json");
  }
  let mut request = request
    .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
    .unwrap();
  if let Some(principal) = principal {
    request.extensions_mut().insert(principal);
  }
  let response = app.clone().oneshot(request).await.unwrap();
  let header = |name| {
    response
      .headers()
      .get(name)
      .map(|v: &header::HeaderValue| v.to_str().unwrap().to_owned())
  };
  let content_type = header(header::CONTENT_TYPE).unwrap_or_default();
  let cache_control = header(header::CACHE_CONTROL);
  let status = response.status();
  let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
  let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
  Answer {
    status,
    content_type,
    cache_control,
    body,
  }
}

async fn get(app: &Router, route: &str) -> Answer {
  call(app, Method::GET, route, operator(), None).await
}

fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
  let deadline = Instant::now() + Duration::from_secs(10);
  while !f() {
    assert!(Instant::now() < deadline, "timed out waiting for {what}");
    std::thread::sleep(Duration::from_millis(1));
  }
}

// ── the guard ───────────────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn unauthorised_is_403() {
  let app = Console::new().router(guard());
  let routes = [
    (Method::GET, "fibers", None),
    (Method::GET, "supervisors", None),
    (Method::GET, "metrics", None),
    (Method::GET, "health", None),
    (Method::GET, "log-level", None),
    (Method::PUT, "log-level", Some(json!({ "level": "trace" }))),
    (Method::GET, "no-such-route", None),
  ];
  for (method, route, body) in routes {
    let denied = call(&app, method.clone(), route, developer(), body.clone()).await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN, "{method} {route}");
    assert_eq!(denied.content_type, "application/problem+json");
    assert_eq!(denied.body["policy"], "console.operate");
    assert_eq!(denied.body["detail"], "access denied");

    let anonymous = call(&app, method.clone(), route, None, body).await;
    assert_eq!(
      anonymous.status,
      StatusCode::UNAUTHORIZED,
      "{method} {route}"
    );
  }
  // The denied PUT changed nothing.
  assert_eq!(global_minimum_log_level(), None);

  // The operator gets through, and an unknown console route is a 404 behind the guard.
  assert_eq!(get(&app, "fibers").await.status, StatusCode::OK);
  let missing = get(&app, "no-such-route").await;
  assert_eq!(missing.status, StatusCode::NOT_FOUND);
  assert_eq!(missing.content_type, "application/problem+json");
}

#[tokio::test(flavor = "multi_thread")]
async fn answers_are_json_and_never_cached() {
  let app = Console::new().router(guard());
  for route in ["fibers", "supervisors", "metrics", "health", "log-level"] {
    let answer = get(&app, route).await;
    assert_eq!(answer.status, StatusCode::OK, "{route}");
    assert_eq!(answer.content_type, "application/json", "{route}");
    assert_eq!(answer.cache_control.as_deref(), Some("no-store"), "{route}");
  }
}

// ── fibers ──────────────────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn fiber_dump_lists_running_fiber() {
  let (tx, rx) = mpsc::channel();
  let running = Runtime::global().fork(
    sync::<_, Never, HNil>(move || tx.send(()).unwrap())
      .then(sleep(Duration::from_secs(3600)))
      .with_span("console.test.work"),
  );
  rx.recv_timeout(Duration::from_secs(10))
    .expect("forked fiber never ran");
  let id = running.fiber().id().as_u64();

  let app = Console::new().router(guard());
  let answer = get(&app, "fibers").await;
  assert_eq!(answer.status, StatusCode::OK);
  let fibers: Vec<FiberView> = serde_json::from_value(answer.body).unwrap();
  let fiber = fibers
    .iter()
    .find(|f| f.id == id)
    .expect("running fiber missing from the dump");
  assert_eq!(fiber.state, "running");
  assert_eq!(fiber.parent, None);
  assert_eq!(fiber.spans, ["console.test.work"]);
  assert!(fibers.windows(2).all(|w| w[0].id < w[1].id), "sorted by id");

  running.interrupt_now();
  wait_until("the fiber to finish", || running.fiber().is_done());
  let fibers: Vec<FiberView> = serde_json::from_value(get(&app, "fibers").await.body).unwrap();
  assert!(
    fibers.iter().all(|f| f.id != id),
    "a finished fiber stays listed"
  );
}

// ── supervisors ─────────────────────────────────────────────────────────────────────────────

/// A worker that only waits.
struct Idle;

impl Process for Idle {
  type Args = ();
  type Env = HNil;
  type Call = ();
  type Reply = ();
  type Cast = ();
  type Info = ();
  type Error = String;

  fn init(_args: (), _ctx: ProcessCtx) -> Effect<Self, String, HNil> {
    succeed(Idle)
  }

  fn handle_call(self, _msg: (), _ctx: ProcessCtx) -> Effect<((), Next<Self>), String, HNil> {
    succeed(((), Next::Continue(self)))
  }
}

fn idle(id: &str) -> ChildSpec {
  ChildSpec::worker::<Idle, _>(id, || ((), HNil))
}

/// A root supervisor (workers `a`, `b`, then the nested supervisor `inner` with worker `c`) and
/// the nested supervisor's handle. The nested one is started on its own and linked in, so the
/// test holds its handle as an app would.
async fn tree(node: &ProcessNode) -> (SupervisorHandle, SupervisorHandle) {
  let slot: Arc<Mutex<Option<SupervisorHandle>>> = Arc::default();
  let inner_slot = Arc::clone(&slot);
  let start_inner: StartFn = Arc::new(move |node, supervisor| {
    let inner = SupervisorSpec::new(Strategy::OneForOne)
      .child(idle("c"))
      .start(node);
    node.link(supervisor, inner.pid());
    let pid = inner.pid().clone();
    *inner_slot.lock().unwrap() = Some(inner);
    Ok(pid)
  });
  let root = SupervisorSpec::new(Strategy::OneForOne)
    .child(idle("a"))
    .child(idle("b"))
    .child(ChildSpec::from_start("inner", start_inner))
    .start(node);
  // The root's init starts the children, so it has answered once its children are listed.
  assert_eq!(which_children(&root).await.len(), 3);
  let inner = slot
    .lock()
    .unwrap()
    .clone()
    .expect("inner supervisor started");
  (root, inner)
}

async fn which_children(handle: &SupervisorHandle) -> Vec<(String, Option<Pid>)> {
  match EffectExecutor::global()
    .run_effect(handle.which_children::<HNil>(T))
    .await
  {
    Exit::Success(children) => children,
    Exit::Failure(cause) => panic!("which_children failed: {cause:?}"),
  }
}

fn assert_children(node: &TreeNodeView, expected: &[(String, Option<Pid>)]) {
  let got: Vec<(&str, Option<&str>)> = node
    .children
    .iter()
    .map(|c| (c.id.as_str(), c.pid.as_deref()))
    .collect();
  let want: Vec<(String, Option<String>)> = expected
    .iter()
    .map(|(id, pid)| (id.clone(), pid.as_ref().map(ToString::to_string)))
    .collect();
  let want: Vec<(&str, Option<&str>)> = want
    .iter()
    .map(|(id, pid)| (id.as_str(), pid.as_deref()))
    .collect();
  assert_eq!(got, want, "children of {}", node.id);
}

#[tokio::test(flavor = "multi_thread")]
async fn supervisor_tree_matches_which_children() {
  let node = ProcessNode::new(NodeName::new("console@test"));
  let (root, inner) = tree(&node).await;
  let app = Console::new()
    .supervisor("root", root.clone())
    .supervisor("inner", inner.clone())
    .router(guard());

  let answer = get(&app, "supervisors").await;
  assert_eq!(answer.status, StatusCode::OK);
  let view: SupervisorsView = serde_json::from_value(answer.body).unwrap();
  assert!(view.unreachable.is_empty());
  assert_eq!(view.roots.len(), 1, "the nested supervisor is not a root");

  let top = &view.roots[0];
  assert_eq!(top.id, "root");
  assert_eq!(top.pid.as_deref(), Some(root.pid().to_string().as_str()));
  assert_children(top, &which_children(&root).await);

  let nested = &top.children[2];
  assert_eq!(nested.id, "inner");
  assert_eq!(
    nested.pid.as_deref(),
    Some(inner.pid().to_string().as_str())
  );
  assert_children(nested, &which_children(&inner).await);
  assert_eq!(nested.children[0].id, "c");
  assert!(top.children[0].children.is_empty(), "a worker is a leaf");
}

#[tokio::test(flavor = "multi_thread")]
async fn stopped_supervisor_is_reported_unreachable() {
  let node = ProcessNode::new(NodeName::new("console@stopped"));
  let live = SupervisorSpec::new(Strategy::OneForOne)
    .child(idle("w"))
    .start(&node);
  let stopped = SupervisorSpec::new(Strategy::OneForOne).start(&node);
  let stop = stopped.stop::<Never, HNil>(&node, ShutdownPolicy::BrutalKill);
  assert!(matches!(
    EffectExecutor::global().run_effect(stop).await,
    Exit::Success(())
  ));
  let app = Console::new()
    .supervisor("live", live.clone())
    .supervisor("stopped", stopped.clone())
    .call_timeout(Duration::from_millis(500))
    .router(guard());

  let view: SupervisorsView = serde_json::from_value(get(&app, "supervisors").await.body).unwrap();
  assert_eq!(view.roots.len(), 1);
  assert_eq!(view.roots[0].id, "live");
  assert_children(&view.roots[0], &which_children(&live).await);
  assert_eq!(view.unreachable.len(), 1);
  assert_eq!(view.unreachable[0].id, "stopped");
  assert_eq!(view.unreachable[0].pid, stopped.pid().to_string());
  assert!(!view.unreachable[0].error.is_empty());
}

// ── metrics and health ──────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn metrics_list_the_registry_then_the_runtime_gauges() {
  let registry = MetricRegistry::new();
  registry.counter("console.requests").update_now(3);
  let app = Console::new().metrics(registry).router(guard());

  let metrics: Vec<MetricView> = serde_json::from_value(get(&app, "metrics").await.body).unwrap();
  let names: Vec<&str> = metrics.iter().map(|m| m.name.as_str()).collect();
  assert_eq!(
    names,
    [
      "console.requests",
      "id_effect_fibers_live",
      "id_effect_fibers_started_total"
    ]
  );
  assert_eq!(metrics[0].kind, "counter");
  assert_eq!(metrics[0].value, json!({ "count": 3 }));
  assert_eq!(metrics[1].kind, "gauge");
}

#[tokio::test(flavor = "multi_thread")]
async fn health_reports_every_check_with_200_even_when_down() {
  let health = HealthRegistry::new()
    .with_indicator(HealthIndicator::new("db", || {
      succeed::<_, Never, HNil>(HealthOutcome::up())
    }))
    .with_indicator(HealthIndicator::new("cache", || {
      succeed::<_, Never, HNil>(HealthOutcome::down("unreachable"))
    }));
  let app = Console::new().health(health).router(guard());

  let answer = get(&app, "health").await;
  assert_eq!(answer.status, StatusCode::OK);
  assert_eq!(answer.body["status"], "down");
  assert_eq!(answer.body["checks"]["db"]["status"], "up");
  assert_eq!(answer.body["checks"]["cache"]["detail"], "unreachable");
}

// ── log level ───────────────────────────────────────────────────────────────────────────────

/// `io::Write` into a shared buffer, for the `tracing` fmt layer.
#[derive(Clone, Default)]
struct SharedBuf(Arc<Mutex<Vec<u8>>>);

impl io::Write for SharedBuf {
  fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
    self.0.lock().unwrap().extend_from_slice(buf);
    Ok(buf.len())
  }
  fn flush(&mut self) -> io::Result<()> {
    Ok(())
  }
}

impl SharedBuf {
  fn text(&self) -> String {
    String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
  }
}

/// Logs `tag` at debug through `tracing` and through an `EffectLogger` whose fiber-local minimum
/// is `Info`.
fn log_debug(tag: &str) {
  tracing::debug!("tracing-{tag}");
  let program: Effect<(), EffectLoggerError, HNil> = EffectLogger.debug(format!("logger-{tag}"));
  let exit = program
    .provide_layer(provide_minimum_log_level(LogLevel::Info))
    .block_on();
  assert!(matches!(exit, Exit::Success(())));
}

#[test]
fn log_level_change_takes_effect_without_restart() {
  let out = SharedBuf::default();
  let (filter, handle) = reload::Layer::<EnvFilter, Registry>::new(EnvFilter::new("info"));
  let writer = out.clone();
  let subscriber = Registry::default().with(filter).with(
    tracing_subscriber::fmt::layer()
      .with_ansi(false)
      .with_writer(move || writer.clone()),
  );
  tracing::subscriber::set_global_default(subscriber).expect("one subscriber per test process");
  let app = Console::new().env_filter(handle).router(guard());
  let rt = tokio::runtime::Runtime::new().unwrap();

  let before: LogLevelView =
    serde_json::from_value(rt.block_on(get(&app, "log-level")).body).unwrap();
  assert_eq!(
    before,
    LogLevelView {
      filter: Some("info".to_owned()),
      logger: None,
    }
  );
  log_debug("before");
  assert!(!out.text().contains("before"), "{}", out.text());

  let put = |body: Value| rt.block_on(call(&app, Method::PUT, "log-level", operator(), Some(body)));
  let changed = put(json!({ "level": "debug" }));
  assert_eq!(changed.status, StatusCode::OK);
  let after: LogLevelView = serde_json::from_value(changed.body).unwrap();
  assert_eq!(after.filter.as_deref(), Some("debug"));
  assert_eq!(after.logger.as_deref(), Some("debug"));
  log_debug("after");
  let text = out.text();
  assert!(text.contains("tracing-after"), "{text}");
  assert!(text.contains("logger-after"), "{text}");

  // A full directive for the filter; the logger minimum stays.
  let narrowed = put(json!({ "filter": "warn" }));
  assert_eq!(narrowed.status, StatusCode::OK);
  assert_eq!(
    narrowed.body,
    json!({ "filter": "warn", "logger": "debug" })
  );
  log_debug("narrowed");
  assert!(!out.text().contains("narrowed"));

  // Invalid changes are 400 and change nothing.
  for bad in [
    json!({ "level": "loud" }),
    json!({}),
    json!({ "filter": "[[[" }),
  ] {
    let rejected = put(bad.clone());
    assert_eq!(rejected.status, StatusCode::BAD_REQUEST, "{bad}");
    assert_eq!(rejected.content_type, "application/problem+json");
  }
  let malformed = rt.block_on(async {
    let request = Request::builder()
      .method(Method::PUT)
      .uri(format!("{API_PREFIX}/log-level"))
      .header(header::CONTENT_TYPE, "application/json")
      .body(Body::from("not json"))
      .unwrap();
    let mut request = request;
    request.extensions_mut().insert(operator().unwrap());
    app.clone().oneshot(request).await.unwrap().status()
  });
  assert_eq!(malformed, StatusCode::BAD_REQUEST);
  let unchanged: LogLevelView =
    serde_json::from_value(rt.block_on(get(&app, "log-level")).body).unwrap();
  assert_eq!(unchanged.filter.as_deref(), Some("warn"));
  assert_eq!(unchanged.logger.as_deref(), Some("debug"));
  assert_eq!(global_minimum_log_level(), Some(LogLevel::Debug));
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_filter_handle_only_the_logger_level_changes() {
  let app = Console::new().router(guard());
  let changed = call(
    &app,
    Method::PUT,
    "log-level",
    operator(),
    Some(json!({ "level": "warn" })),
  )
  .await;
  assert_eq!(changed.status, StatusCode::OK);
  assert_eq!(changed.body, json!({ "filter": null, "logger": "warn" }));
  let refused = call(
    &app,
    Method::PUT,
    "log-level",
    operator(),
    Some(json!({ "filter": "debug" })),
  )
  .await;
  assert_eq!(refused.status, StatusCode::BAD_REQUEST);
}
