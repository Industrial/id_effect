//! `database`: when `DATABASE_URL` is set (environment or `.env`), its host accepts a TCP
//! connection; a value only in the environment is ignored when `.env.example` exists and does
//! not list the key (the project does not use a database). Only reachability is checked: no
//! login, no query. The URL is never printed whole (it may hold a password), only `host:port`.

use std::time::Duration;

use super::Check;
use super::project::{Project, Source};
use super::system::System;

const NAME: &str = "database";
const TIMEOUT: Duration = Duration::from_secs(3);

/// Where a database URL points.
#[derive(Debug, PartialEq, Eq)]
enum Target {
  /// A TCP `host:port`.
  Tcp(String, u16),
  /// Not over TCP (SQLite, a Unix socket); why.
  Local(&'static str),
}

pub(crate) fn check(project: &Project, sys: &dyn System) -> Check {
  let Some((url, source)) = project.var(sys, "DATABASE_URL") else {
    return Check::skip(NAME, "DATABASE_URL is not set (environment or .env)");
  };
  if url.trim().is_empty() {
    return Check::skip(NAME, format!("DATABASE_URL is empty ({})", source.label()));
  }
  // A shell-wide DATABASE_URL says nothing about a project whose .env.example omits it.
  let listed = |pairs: &[(String, String)]| pairs.iter().any(|(k, _)| k == "DATABASE_URL");
  if source == Source::Environment
    && !listed(&project.dotenv)
    && project.example.as_deref().is_some_and(|e| !listed(e))
  {
    return Check::skip(
      NAME,
      "DATABASE_URL is set in the environment, but .env.example does not list it",
    );
  }
  match target(&url) {
    Err(reason) => Check::fail(NAME, format!("DATABASE_URL ({}): {reason}", source.label())),
    Ok(Target::Local(what)) => Check::skip(NAME, format!("DATABASE_URL is {what}, not TCP")),
    Ok(Target::Tcp(host, port)) => match sys.connect(&host, port, TIMEOUT) {
      Ok(()) => Check::pass(NAME, format!("{host}:{port} accepts TCP connections")),
      Err(e) => Check::fail(
        NAME,
        format!(
          "cannot reach {host}:{port} (DATABASE_URL from {}): {e}; is the database running (`docker compose up -d db`)?",
          source.label()
        ),
      ),
    },
  }
}

/// The TCP target of `scheme://[user[:password]@]host[:port][/db][?…]`, with the scheme's
/// default port when none is given.
fn target(url: &str) -> Result<Target, String> {
  let Some((scheme, rest)) = url.split_once("://") else {
    return Err("not a URL (expected scheme://host:port/…)".into());
  };
  let scheme = scheme.to_ascii_lowercase();
  if scheme.starts_with("sqlite") || scheme == "file" {
    return Ok(Target::Local("a SQLite file"));
  }
  let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
  let host_port = authority.rsplit_once('@').map_or(authority, |(_, hp)| hp);
  let (host, port) = if let Some(bracketed) = host_port.strip_prefix('[') {
    let (host, after) = bracketed
      .split_once(']')
      .ok_or("unclosed `[` in the host")?;
    (host, after.strip_prefix(':'))
  } else {
    match host_port.rsplit_once(':') {
      Some((host, port)) => (host, Some(port)),
      None => (host_port, None),
    }
  };
  if host.is_empty() {
    return Ok(Target::Local("a Unix socket (no host)"));
  }
  let port = match port {
    Some(port) => port
      .parse()
      .map_err(|_| format!("`{port}` is not a port number"))?,
    None => default_port(&scheme)
      .ok_or_else(|| format!("no port, and `{scheme}` has no default port doctor knows"))?,
  };
  Ok(Target::Tcp(host.to_string(), port))
}

fn default_port(scheme: &str) -> Option<u16> {
  match scheme {
    "postgres" | "postgresql" => Some(5432),
    "mysql" | "mariadb" => Some(3306),
    "redis" | "rediss" => Some(6379),
    "mongodb" => Some(27017),
    _ => None,
  }
}

#[cfg(test)]
mod tests {
  use std::fs;
  use std::io::ErrorKind;

  use super::super::Status;
  use super::super::fake::FakeSystem;
  use super::*;

  fn project() -> (tempfile::TempDir, Project) {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("Cargo.toml"), "[package]\nname = \"a\"\n").unwrap();
    let project = Project::load(tmp.path()).unwrap();
    (tmp, project)
  }

  fn tcp(host: &str, port: u16) -> Result<Target, String> {
    Ok(Target::Tcp(host.into(), port))
  }

  #[test]
  fn urls_resolve_to_host_and_port() {
    assert_eq!(target("postgres://u:p@db:6543/app"), tcp("db", 6543));
    assert_eq!(target("postgresql://localhost/app"), tcp("localhost", 5432));
    assert_eq!(
      target("postgres://u:p%40ss@host?sslmode=require"),
      tcp("host", 5432)
    );
    assert_eq!(target("mysql://[::1]:3307/x"), tcp("::1", 3307));
    assert_eq!(target("redis://[::1]"), tcp("::1", 6379));
    assert_eq!(
      target("sqlite://data.db"),
      Ok(Target::Local("a SQLite file"))
    );
    assert_eq!(
      target("postgres:///app?host=/run/postgresql"),
      Ok(Target::Local("a Unix socket (no host)"))
    );
    assert!(target("localhost:5432").is_err());
    assert!(target("postgres://db:x/app").is_err());
    assert!(target("foo://db/app").is_err());
  }

  #[test]
  fn a_reachable_database_passes() {
    let (_tmp, project) = project();
    let sys = FakeSystem::default().var("DATABASE_URL", "postgres://u:secret@db.local:5432/app");
    let check = check(&project, &sys);
    assert_eq!(check.status, Status::Pass);
    assert_eq!(check.reason, "db.local:5432 accepts TCP connections");
    assert_eq!(*sys.sockets.borrow(), ["db.local:5432"]);
  }

  #[test]
  fn an_unreachable_database_fails_without_leaking_the_password() {
    let (_tmp, project) = project();
    let sys = FakeSystem::default()
      .var("DATABASE_URL", "postgres://u:secret@localhost/app")
      .refuse_connect(ErrorKind::ConnectionRefused);
    let check = check(&project, &sys);
    assert_eq!(check.status, Status::Fail);
    assert!(
      check
        .reason
        .starts_with("cannot reach localhost:5432 (DATABASE_URL from environment)")
    );
    assert!(!check.reason.contains("secret"));
  }

  #[test]
  fn dotenv_supplies_the_url_and_a_bad_one_fails() {
    let (tmp, _) = project();
    fs::write(tmp.path().join(".env"), "DATABASE_URL=not a url\n").unwrap();
    let project = Project::load(tmp.path()).unwrap();
    let check = check(&project, &FakeSystem::default());
    assert_eq!(check.status, Status::Fail);
    assert!(check.reason.starts_with("DATABASE_URL (.env): not a URL"));
  }

  #[test]
  fn an_environment_url_the_project_does_not_list_is_ignored() {
    let (tmp, _) = project();
    fs::write(tmp.path().join(".env.example"), "PORT=8080\n").unwrap();
    let project = Project::load(tmp.path()).unwrap();
    let sys = FakeSystem::default()
      .var("DATABASE_URL", "postgres://localhost/other")
      .refuse_connect(ErrorKind::ConnectionRefused);
    let check = check(&project, &sys);
    assert_eq!(check.status, Status::Skip);
    assert!(sys.sockets.borrow().is_empty());
    fs::write(tmp.path().join(".env.example"), "DATABASE_URL=\n").unwrap();
    let listed = Project::load(tmp.path()).unwrap();
    assert_eq!(super::check(&listed, &sys).status, Status::Fail);
  }

  #[test]
  fn unset_empty_or_local_urls_skip() {
    let (_tmp, project) = project();
    let skip = |sys: FakeSystem| check(&project, &sys).status;
    assert_eq!(skip(FakeSystem::default()), Status::Skip);
    assert_eq!(
      skip(FakeSystem::default().var("DATABASE_URL", "")),
      Status::Skip
    );
    assert_eq!(
      skip(FakeSystem::default().var("DATABASE_URL", "sqlite://x.db")),
      Status::Skip
    );
  }
}
