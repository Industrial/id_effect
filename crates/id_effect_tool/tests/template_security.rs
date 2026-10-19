//! The `service` template's operator surfaces and layer order (ADR 0013 R4, R7; findings M4,
//! M6, L2), checked on the generated sources. The generated project's own tests
//! (`tests/http.rs`: `m4_*`, `m6_*`, `l2_*`) check the behaviour; `scripts/template-smoke.sh`
//! runs them.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// `id-effect new demo --template service ARGS` into `dir/name`; returns the project directory.
fn service(dir: &Path, name: &str, args: &[&str]) -> PathBuf {
  let dest = dir.join(name);
  let out = Command::new(env!("CARGO_BIN_EXE_id-effect"))
    .args(["new", "demo", "--template", "service", "--dest"])
    .arg(&dest)
    .args(args)
    .output()
    .expect("spawn id-effect");
  assert!(out.status.success(), "{out:?}");
  dest
}

fn read(path: PathBuf) -> String {
  fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The byte offset of `needle` in `text`, which must contain it exactly once.
fn position(text: &str, needle: &str) -> usize {
  assert_eq!(text.matches(needle).count(), 1, "{needle} once in:\n{text}");
  text.find(needle).unwrap()
}

#[test]
fn m4_template_limit_layers_wrap_security_layer() {
  let tmp = tempfile::tempdir().unwrap();
  for (name, args) in [
    ("plain", &[][..]),
    ("otel", &["--otel"][..]),
    ("console", &["--jobs", "--console"][..]),
  ] {
    let lib = read(service(tmp.path(), name, args).join("src/lib.rs"));
    // `Router::layer` wraps what came before: a later layer is an outer one.
    // (`.csrf_exempt("/rpc")` follows it: RPC needs `x-rpc` and a JSON body instead, C5.)
    let security = position(&lib, ".layer(SecurityLayer::recommended(cookie_key)");
    let body_limit = position(&lib, ".layer(BodyLimitLayer::new(BODY_LIMIT))");
    let timeout = position(&lib, ".layer(TimeoutLayer::new(REQUEST_TIMEOUT))");
    assert!(
      security < body_limit,
      "{name}: BodyLimitLayer inside SecurityLayer"
    );
    assert!(
      security < timeout,
      "{name}: TimeoutLayer inside SecurityLayer"
    );
    let tests = read(tmp.path().join(name).join("tests/http.rs"));
    assert!(tests.contains("async fn m4_template_limit_layers_wrap_security_layer()"));
  }
}

#[test]
fn l2_template_metrics_not_public() {
  let tmp = tempfile::tempdir().unwrap();
  for (name, args) in [("plain", &[][..]), ("console", &["--console"][..])] {
    let dir = service(tmp.path(), name, args);
    let lib = read(dir.join("src/lib.rs"));
    assert!(
      !lib.contains("metrics_router()"),
      "{name}: unguarded /metrics"
    );
    position(
      &lib,
      ".merge(guarded_metrics_router(MetricsToken::from_env()))",
    );
    assert!(lib.contains("impl RouteGuard for MetricsToken"));
    let tests = read(dir.join("tests/http.rs"));
    assert!(tests.contains("async fn l2_template_metrics_not_public()"));
  }
}

#[test]
fn m6_loopback_operator_only_in_dev() {
  let tmp = tempfile::tempdir().unwrap();
  for (name, args) in [
    ("console", &["--console"][..]),
    ("jobs-console", &["--jobs", "--console"][..]),
  ] {
    let dir = service(tmp.path(), name, args);
    let console = read(dir.join("src/console.rs"));
    // The shortcut is layered in one place, under the dev switch.
    let shortcut = position(
      &console,
      "console.layer(middleware::from_fn(local_operator))",
    );
    let switch = position(&console, "    if dev {\n");
    assert!(switch < shortcut, "{name}");
    assert!(console.contains("pub fn dev_profile() -> bool"));
    assert!(console.contains("pub const PROFILE_ENV: &str = \"ID_EFFECT_PROFILE\";"));
    // The listener checks Host and Origin against CONSOLE_ALLOWED_HOSTS (default: the bind
    // address).
    assert!(console.contains("let routes = config.check_host_and_origin(routes);"));
    assert!(console.contains("pub const ALLOWED_HOSTS_ENV: &str = \"CONSOLE_ALLOWED_HOSTS\";"));
    let tests = read(dir.join("tests/http.rs"));
    assert!(tests.contains("async fn m6_loopback_operator_only_in_dev()"));
    assert!(tests.contains("async fn m6_console_rejects_foreign_host_and_origin()"));
  }
  // The deployed service runs the prod profile.
  let deploy = read(tmp.path().join("console/config/deploy.yml"));
  assert!(deploy.contains("ID_EFFECT_PROFILE: \"prod\""), "{deploy}");
}
