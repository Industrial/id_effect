//! The console's Host and Origin check (M6): on its own listener the console answers only its
//! allowed hosts (default: the bind address) and same-origin browsers; anything else is a `403`
//! Problem Details answer before the guard runs.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::middleware;
use axum::response::Response;
use id_effect_authz::{BasicPrincipal, CurrentPrincipal, Guard, require_role};
use id_effect_console::{Console, ConsoleConfig};
use tower::ServiceExt;

/// The console with every request an operator, behind `config`'s check.
fn console(config: &ConsoleConfig) -> Router {
  let routes = Console::new()
    .router(Guard::new("console", require_role("operator")))
    .layer(middleware::from_fn(
      |mut request: axum::extract::Request, next: middleware::Next| async move {
        let operator = BasicPrincipal::new("op").with_role("operator");
        request
          .extensions_mut()
          .insert(CurrentPrincipal::new(operator));
        next.run(request).await
      },
    ));
  config.check_host_and_origin(routes)
}

async fn get(app: &Router, headers: &[(&str, &str)]) -> Response {
  let mut request = Request::get("/_console/api/health");
  for (name, value) in headers {
    request = request.header(*name, *value);
  }
  app
    .clone()
    .oneshot(request.body(Body::empty()).unwrap())
    .await
    .unwrap()
}

#[tokio::test]
async fn m6_console_rejects_foreign_host_and_origin() {
  let app = console(&ConsoleConfig::loopback(9090));
  for host in ["127.0.0.1:9090", "localhost:9090", "LocalHost:9090"] {
    let answer = get(&app, &[("host", host)]).await;
    assert_eq!(answer.status(), StatusCode::OK, "{host}");
  }
  let same_origin = get(
    &app,
    &[
      ("host", "127.0.0.1:9090"),
      ("origin", "http://127.0.0.1:9090"),
    ],
  )
  .await;
  assert_eq!(same_origin.status(), StatusCode::OK);

  let foreign: [&[(&str, &str)]; 7] = [
    &[],
    &[("host", "rebind.attacker.example:9090")],
    &[("host", "127.0.0.1:9091")],
    &[("host", "127.0.0.1")],
    &[
      ("host", "127.0.0.1:9090"),
      ("origin", "https://attacker.example"),
    ],
    &[("host", "127.0.0.1:9090"), ("origin", "null")],
    &[
      ("host", "127.0.0.1:9090"),
      ("origin", "http://127.0.0.1:9091"),
    ],
  ];
  for headers in foreign {
    let answer = get(&app, headers).await;
    assert_eq!(answer.status(), StatusCode::FORBIDDEN, "{headers:?}");
    assert_eq!(
      answer.headers()["content-type"],
      "application/problem+json",
      "{headers:?}"
    );
  }
  // The fallback is behind the check too.
  let unknown = app
    .clone()
    .oneshot(
      Request::get("/_console/api/nothing")
        .header("host", "attacker.example")
        .body(Body::empty())
        .unwrap(),
    )
    .await
    .unwrap();
  assert_eq!(unknown.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn allowed_hosts_replace_the_bind_address() {
  let config = ConsoleConfig::loopback(9090).allow_hosts(["console.internal", " "]);
  assert_eq!(config.allowed_hosts(), ["console.internal"]);
  let app = console(&config);
  let tunnel = get(
    &app,
    &[
      ("host", "console.internal:8443"),
      ("origin", "https://console.internal:8443"),
    ],
  )
  .await;
  assert_eq!(tunnel.status(), StatusCode::OK);
  let bind = get(&app, &[("host", "127.0.0.1:9090")]).await;
  assert_eq!(bind.status(), StatusCode::FORBIDDEN);
}

#[test]
fn default_hosts_follow_the_bind_address() {
  let config = |bind: &str| ConsoleConfig::new(bind.parse().unwrap());
  assert_eq!(
    config("[::1]:9090").allowed_hosts(),
    ["[::1]:9090", "localhost:9090"]
  );
  assert_eq!(config("10.0.0.5:9090").allowed_hosts(), ["10.0.0.5:9090"]);
  let any_port = config("127.0.0.1:0");
  assert_eq!(any_port.allowed_hosts(), ["127.0.0.1", "localhost"]);
  assert!(any_port.allows_host("127.0.0.1:41234"));
  assert!(config("[::1]:9090").allows_host("[::1]:9090"));
  assert!(!config("[::1]:9090").allows_host("[::2]:9090"));
  // An unspecified bind matches no real Host: name the hosts explicitly.
  assert!(!config("0.0.0.0:9090").allows_host("console.example:9090"));
}
