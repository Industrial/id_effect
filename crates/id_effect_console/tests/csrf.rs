//! The dashboard behind `id_effect_axum`'s `SecurityLayer::recommended`: the page carries the
//! session's CSRF token, and its state-changing calls (a JSON body, the `X-CSRF-Token` header and
//! the session's CSRF cookie) pass the CSRF check; the same call without the header is refused.

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use id_effect_authz::{BasicPrincipal, CurrentPrincipal, Guard, require_role};
use id_effect_axum::server::security::{CSRF_HEADER, CookieKey, SecurityLayer};
use id_effect_console::Console;
use tower::ServiceExt;

fn guard() -> Guard {
  Guard::new("console.operate", require_role("operator"))
}

/// The console under the recommended security layer, as an app mounts it.
fn secured(console: Console) -> Router {
  console
    .router(guard())
    .layer(SecurityLayer::recommended(CookieKey::generate()))
}

struct Reply {
  status: StatusCode,
  headers: HeaderMap,
  body: String,
}

/// One operator request; `cookie` and `csrf` are what the browser and the page's script send.
async fn send(
  app: &Router,
  method: Method,
  path: &str,
  cookie: Option<&str>,
  csrf: Option<&str>,
  json: Option<&str>,
) -> Reply {
  let mut request = Request::builder().method(method).uri(path);
  if let Some(cookie) = cookie {
    request = request.header(header::COOKIE, cookie);
  }
  if let Some(csrf) = csrf {
    request = request.header(CSRF_HEADER, csrf);
  }
  if json.is_some() {
    request = request
      .header(header::CONTENT_TYPE, "application/json")
      .header(header::ACCEPT, "application/json");
  }
  let mut request = request
    .body(json.map_or_else(Body::empty, |b| Body::from(b.to_owned())))
    .unwrap();
  request.extensions_mut().insert(CurrentPrincipal::new(
    BasicPrincipal::new("ops").with_role("operator"),
  ));
  let response = app.clone().oneshot(request).await.unwrap();
  let status = response.status();
  let headers = response.headers().clone();
  let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
  Reply {
    status,
    headers,
    body: String::from_utf8(bytes.to_vec()).unwrap(),
  }
}

/// The value of the page's `<meta name="csrf-token">`, as the page's script reads it.
fn meta_token(page: &str) -> &str {
  let start = page
    .find(r#"<meta name="csrf-token" content=""#)
    .expect("a csrf meta tag")
    + r#"<meta name="csrf-token" content=""#.len();
  let len = page[start..].find('"').unwrap();
  &page[start..start + len]
}

/// Loads the page the way a browser does: the session's CSRF cookie (`name=value`, sent back by
/// the browser) and the token the page's script echoes.
async fn open_dashboard(app: &Router) -> (String, String) {
  let page = send(app, Method::GET, "/_console", None, None, None).await;
  assert_eq!(page.status, StatusCode::OK, "{}", page.body);
  let set_cookie = page
    .headers
    .get(header::SET_COOKIE)
    .expect("the security layer issues the CSRF cookie")
    .to_str()
    .unwrap();
  let cookie = set_cookie.split(';').next().unwrap().to_owned();
  assert!(cookie.starts_with("__Host-csrf="), "{set_cookie}");
  let token = meta_token(&page.body).to_owned();
  assert!(!token.is_empty(), "the page carries no CSRF token");
  (cookie, token)
}

#[tokio::test(flavor = "multi_thread")]
async fn log_level_put_passes_the_csrf_check_only_with_the_header() {
  let app = secured(Console::new());
  let (cookie, token) = open_dashboard(&app).await;
  let put = |csrf: Option<String>, level: &'static str| {
    let (app, cookie) = (app.clone(), cookie.clone());
    async move {
      let body = format!(r#"{{"level":"{level}"}}"#);
      send(
        &app,
        Method::PUT,
        "/_console/api/log-level",
        Some(&cookie),
        csrf.as_deref(),
        Some(&body),
      )
      .await
    }
  };

  // The page's request: JSON, the session cookie and the token from its meta tag.
  let ok = put(Some(token.clone()), "warn").await;
  assert_eq!(ok.status, StatusCode::OK, "{}", ok.body);
  assert!(ok.body.contains(r#""logger":"warn""#), "{}", ok.body);

  // Without the header (the page before 0.10), or with another token: refused, nothing changed.
  for csrf in [None, Some("forged".to_owned())] {
    let refused = put(csrf.clone(), "trace").await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{csrf:?}");
    assert_eq!(refused.body, "csrf token mismatch");
  }
  let now = send(
    &app,
    Method::GET,
    "/_console/api/log-level",
    Some(&cookie),
    None,
    None,
  )
  .await;
  assert!(now.body.contains(r#""logger":"warn""#), "{}", now.body);
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_csrf_layer_the_meta_tag_stays_empty() {
  let app = Console::new().router(guard());
  let page = send(&app, Method::GET, "/_console", None, None, None).await;
  assert_eq!(page.status, StatusCode::OK);
  assert_eq!(meta_token(&page.body), "");
}

#[cfg(feature = "jobs")]
mod jobs {
  use std::time::Duration;

  use axum::http::{Method, StatusCode};
  use id_effect::{Effect, Exit, WallClock};
  use id_effect_axum::EffectExecutor;
  use id_effect_console::Console;
  use id_effect_jobs::{
    JobError, JobFailure, JobQueue, JobStore, MemoryJobQueue, NewJob, Priority, Queue, RunAt,
  };
  use serde_json::json;

  use super::{open_dashboard, secured, send};

  async fn run<A: Send + 'static>(effect: Effect<A, JobError>) -> A {
    match EffectExecutor::global().run_effect(effect).await {
      Exit::Success(a) => a,
      Exit::Failure(cause) => panic!("queue operation failed: {cause:?}"),
    }
  }

  #[tokio::test(flavor = "multi_thread")]
  async fn job_retry_post_passes_the_csrf_check_only_with_the_header() {
    let queue = MemoryJobQueue::new(WallClock::system());
    let id = run(queue.enqueue_raw(NewJob {
      name: "app.mail".to_owned(),
      queue: Queue::default(),
      payload: json!({}),
      priority: Priority::NORMAL,
      max_attempts: 1,
      unique: None,
      run_at: RunAt::Now,
    }))
    .await
    .id()
    .unwrap();
    let claimed = run(queue.claim(&Queue::default(), "w", 1, Duration::from_secs(60))).await;
    assert_eq!(claimed[0].id, id);
    run(queue.fail(id, "w", &JobFailure::discard("smtp refused"), None)).await;

    let app = secured(Console::new().jobs(queue));
    let (cookie, token) = open_dashboard(&app).await;
    let path = format!("/_console/api/jobs/{}/retry", id.0);

    let refused = send(&app, Method::POST, &path, Some(&cookie), None, Some("{}")).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    let ok = send(
      &app,
      Method::POST,
      &path,
      Some(&cookie),
      Some(&token),
      Some("{}"),
    )
    .await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.body);
    assert!(ok.body.contains(r#""state":"scheduled""#), "{}", ok.body);
  }
}
