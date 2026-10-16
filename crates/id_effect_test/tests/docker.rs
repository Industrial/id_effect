//! `require_docker` end to end: this binary re-runs its own `probe` test in a child process
//! whose `DOCKER_HOST` points at a socket that does not exist.

use std::process::Command;

use id_effect_test::{REQUIRE_DOCKER_ENV, docker_available, require_docker};

/// The child's test: reports what `require_docker` decided. Ignored in normal runs.
#[test]
#[ignore = "run by the tests below in a child process"]
fn probe() {
  println!("PROBE-RAN docker={}", require_docker());
}

/// Runs `probe` in a child process without Docker; `required` sets `ID_EFFECT_REQUIRE_DOCKER`.
fn probe_without_docker(required: bool) -> (bool, String) {
  probe_with_docker_host("unix:///nonexistent/id-effect-test.sock", required)
}

/// Runs `probe` in a child process whose `DOCKER_HOST` is `docker_host`.
fn probe_with_docker_host(docker_host: &str, required: bool) -> (bool, String) {
  let mut child = Command::new(std::env::current_exe().expect("test binary"));
  child
    .args(["--exact", "probe", "--include-ignored", "--nocapture"])
    .env("DOCKER_HOST", docker_host)
    .env_remove(REQUIRE_DOCKER_ENV);
  if required {
    child.env(REQUIRE_DOCKER_ENV, "1");
  }
  let out = child.output().expect("run the probe");
  let text = format!(
    "{}{}",
    String::from_utf8_lossy(&out.stdout),
    String::from_utf8_lossy(&out.stderr)
  );
  (out.status.success(), text)
}

#[test]
fn without_docker_a_test_prints_skip_and_passes() {
  let (passed, output) = probe_without_docker(false);
  assert!(passed, "{output}");
  assert!(
    output.contains("SKIP probe: Docker is not available"),
    "{output}"
  );
  assert!(output.contains("PROBE-RAN docker=false"), "{output}");
}

#[test]
fn without_docker_a_required_test_fails() {
  let (passed, output) = probe_without_docker(true);
  assert!(!passed, "{output}");
  assert!(
    output.contains("ID_EFFECT_REQUIRE_DOCKER=1 but Docker is not available"),
    "{output}"
  );
}

#[test]
fn an_unusable_docker_host_is_unavailable_docker_too() {
  let (passed, output) = probe_with_docker_host("bogus://nowhere", false);
  assert!(passed, "{output}");
  assert!(
    output.contains("SKIP probe: Docker is not available"),
    "{output}"
  );
}

#[test]
fn the_probe_is_cached_and_agrees_with_the_guard() {
  let first = docker_available();
  assert_eq!(docker_available(), first);
  if first.is_ok() {
    assert!(require_docker());
  }
}
