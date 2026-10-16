//! `port`: the service port is free. `PORT` and `HOST` come from the environment, `.env`, else
//! `.env.example` (the template's defaults); `HOST` defaults to `0.0.0.0` as `HostConfig` does.
//! Doctor binds `HOST:PORT` and lets go at once.

use std::io::ErrorKind;

use super::Check;
use super::project::Project;
use super::system::System;

const NAME: &str = "port";

pub(crate) fn check(project: &Project, sys: &dyn System) -> Check {
  let Some((port, source)) = project.var_or_example(sys, "PORT") else {
    return Check::skip(NAME, "no PORT in the environment, .env or .env.example");
  };
  let Ok(port) = port.trim().parse::<u16>() else {
    return Check::fail(
      NAME,
      format!("PORT `{port}` ({}) is not a port number", source.label()),
    );
  };
  let host = project
    .var_or_example(sys, "HOST")
    .map(|(host, _)| host)
    .filter(|host| !host.trim().is_empty())
    .unwrap_or_else(|| "0.0.0.0".into());
  if port == 0 {
    return Check::pass(NAME, "PORT is 0: the OS picks a free port");
  }
  let at = format!("{host}:{port} (PORT from {})", source.label());
  match sys.bind(&host, port) {
    Ok(()) => Check::pass(NAME, format!("{at} is free")),
    Err(e) if e.kind() == ErrorKind::AddrInUse => Check::fail(
      NAME,
      format!(
        "{at} is in use: the service is already running, or another program holds it (`ss -ltnp 'sport = :{port}'`)"
      ),
    ),
    Err(e) => Check::warn(NAME, format!("cannot bind {at} to check it: {e}")),
  }
}

#[cfg(test)]
mod tests {
  use std::fs;

  use super::super::Status;
  use super::super::fake::FakeSystem;
  use super::*;

  fn project(example: Option<&str>) -> (tempfile::TempDir, Project) {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("Cargo.toml"), "[package]\nname = \"a\"\n").unwrap();
    if let Some(text) = example {
      fs::write(tmp.path().join(".env.example"), text).unwrap();
    }
    let project = Project::load(tmp.path()).unwrap();
    (tmp, project)
  }

  #[test]
  fn a_free_port_passes_and_the_example_supplies_defaults() {
    let (_tmp, project) = project(Some("HOST=127.0.0.1\nPORT=8080\n"));
    let sys = FakeSystem::default();
    let check = check(&project, &sys);
    assert_eq!(check.status, Status::Pass);
    assert_eq!(
      check.reason,
      "127.0.0.1:8080 (PORT from .env.example) is free"
    );
    assert_eq!(*sys.sockets.borrow(), ["127.0.0.1:8080"]);
  }

  #[test]
  fn the_environment_wins_and_host_defaults_to_all_interfaces() {
    let (_tmp, project) = project(None);
    let sys = FakeSystem::default().var("PORT", "9000");
    assert_eq!(check(&project, &sys).status, Status::Pass);
    assert_eq!(*sys.sockets.borrow(), ["0.0.0.0:9000"]);
  }

  #[test]
  fn a_port_in_use_fails() {
    let (_tmp, project) = project(Some("PORT=8080\n"));
    let sys = FakeSystem::default().refuse_bind(ErrorKind::AddrInUse);
    let check = check(&project, &sys);
    assert_eq!(check.status, Status::Fail);
    assert!(
      check
        .reason
        .starts_with("0.0.0.0:8080 (PORT from .env.example) is in use")
    );
  }

  #[test]
  fn other_bind_errors_warn() {
    let (_tmp, project) = project(Some("PORT=80\n"));
    let sys = FakeSystem::default().refuse_bind(ErrorKind::PermissionDenied);
    assert_eq!(check(&project, &sys).status, Status::Warn);
  }

  #[test]
  fn a_bad_port_fails_port_zero_passes_and_no_port_skips() {
    let (_tmp, project) = project(None);
    let with = |port: &str| check(&project, &FakeSystem::default().var("PORT", port));
    assert_eq!(with("http").status, Status::Fail);
    assert_eq!(with("70000").status, Status::Fail);
    assert_eq!(with("0").status, Status::Pass);
    assert_eq!(check(&project, &FakeSystem::default()).status, Status::Skip);
  }

  #[test]
  fn a_real_listener_makes_the_port_busy() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port().to_string();
    let (_tmp, project) = project(Some("HOST=127.0.0.1\n"));
    let sys = FakeSystem::default().var("PORT", &port).host_sockets();
    let busy = check(&project, &sys);
    assert_eq!(busy.status, Status::Fail, "{busy:?}");
    drop(listener);
    assert_eq!(check(&project, &sys).status, Status::Pass);
  }
}
