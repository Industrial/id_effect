//! `id-effect doctor`: checks that a project can build and run here, each [`Check`] ending
//! [`Status::Pass`], [`Status::Warn`], [`Status::Fail`] or [`Status::Skip`] (not applicable to
//! this project) with its reason.
//!
//! | Check | Fails when | Warns when | Skips when |
//! |---|---|---|---|
//! | `project` | DIR has no `Cargo.toml` (nothing else runs then) | | |
//! | `rustc` | `rustc --version` is older than the project's `rust-version` or the id_effect MSRV, or cannot run | its output is unreadable | |
//! | `versions` | the id_effect crates in `Cargo.lock` resolve to more than one version | there is no `Cargo.lock`, or no id_effect crate in it | |
//! | `env` | a key of `.env.example` is set neither in the environment nor in `.env` | | there is no `.env.example` |
//! | `database` | `DATABASE_URL` is malformed or its host does not accept a TCP connection | | `DATABASE_URL` is unset, not a TCP database, or only in the environment while `.env.example` does not list it |
//! | `docker` | | a `Dockerfile` / compose file is present but `docker` / `docker compose` is not | there is neither |
//! | `port` | `PORT` is not a port number, or `HOST:PORT` is in use | binding is refused for another reason | no `PORT` anywhere |
//! | `graph` (`--graph`) | building or running the binary fails | the binary prints no graph, or one with missing capabilities | |
//!
//! Variables are read from the process environment, then the project's `.env`; `PORT` and
//! `HOST` fall back to `.env.example`. Values are never printed. `docker` checks the CLI only,
//! not whether the daemon runs.
//!
//! [`doctor`] runs the checks on this machine; [`Report::to_text`] and [`Report::to_json`]
//! render them and [`Report::failed`] decides the exit status.

mod database;
mod docker;
mod env;
#[cfg(test)]
pub(crate) mod fake;
mod graph;
mod port;
mod project;
mod rustc;
mod system;
mod versions;

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use project::Project;
use system::{Host, System};

/// A check's outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
  /// Checked and fine.
  Pass,
  /// Checked, and worth a look, but the project can still run.
  Warn,
  /// Checked and broken: `id-effect doctor` exits 1.
  Fail,
  /// Not applicable to this project (e.g. `database` without a `DATABASE_URL`).
  Skip,
}

impl Status {
  /// `pass`, `warn`, `fail` or `skip`.
  pub fn label(self) -> &'static str {
    match self {
      Self::Pass => "pass",
      Self::Warn => "warn",
      Self::Fail => "fail",
      Self::Skip => "skip",
    }
  }
}

/// One check's outcome and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
  /// The check (`rustc`, `versions`, `env`, …).
  pub name: &'static str,
  /// Its outcome.
  pub status: Status,
  /// Why, in one line.
  pub reason: String,
}

impl Check {
  fn new(name: &'static str, status: Status, reason: impl Into<String>) -> Self {
    Self {
      name,
      status,
      reason: reason.into(),
    }
  }

  pub(crate) fn pass(name: &'static str, reason: impl Into<String>) -> Self {
    Self::new(name, Status::Pass, reason)
  }

  pub(crate) fn warn(name: &'static str, reason: impl Into<String>) -> Self {
    Self::new(name, Status::Warn, reason)
  }

  pub(crate) fn fail(name: &'static str, reason: impl Into<String>) -> Self {
    Self::new(name, Status::Fail, reason)
  }

  pub(crate) fn skip(name: &'static str, reason: impl Into<String>) -> Self {
    Self::new(name, Status::Skip, reason)
  }
}

/// How `--graph` asks the binary to print its layer graph.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum GraphFormat {
  /// Text (`--describe-layers`).
  #[default]
  Text,
  /// Graphviz DOT (`--describe-layers=dot`).
  Dot,
}

/// What [`doctor`] does beyond the default checks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Options {
  /// Build and run the binary with `--describe-layers` and keep its layer graph.
  pub graph: Option<GraphFormat>,
}

/// The checks' outcomes for one project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
  /// The project directory.
  pub dir: PathBuf,
  /// Every check, in the order run.
  pub checks: Vec<Check>,
  /// The binary's layer graph, with `--graph` when it printed one.
  pub graph: Option<String>,
}

impl Report {
  /// Some check failed (`id-effect doctor` exits 1).
  pub fn failed(&self) -> bool {
    self.checks.iter().any(|c| c.status == Status::Fail)
  }

  /// How many checks ended `status`.
  pub fn count(&self, status: Status) -> usize {
    self.checks.iter().filter(|c| c.status == status).count()
  }

  /// One line per check, a summary line, then the layer graph when there is one.
  pub fn to_text(&self) -> String {
    let mut out = format!("id-effect doctor: {}\n", self.dir.display());
    let width = self.checks.iter().map(|c| c.name.len()).max().unwrap_or(0);
    for check in &self.checks {
      let _ = writeln!(
        out,
        "  {}  {:width$}  {}",
        check.status.label(),
        check.name,
        check.reason
      );
    }
    let plural =
      |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    let _ = writeln!(
      out,
      "\n{}, {}, {}, {}",
      plural(self.count(Status::Pass), "passed", "passed"),
      plural(self.count(Status::Warn), "warning", "warnings"),
      plural(self.count(Status::Fail), "failed", "failed"),
      plural(self.count(Status::Skip), "skipped", "skipped"),
    );
    if let Some(graph) = &self.graph {
      out.push('\n');
      out.push_str(graph);
    }
    out
  }

  /// The report as one JSON object:
  /// `{"path", "ok", "counts": {"pass", "warn", "fail", "skip"}, "checks": [{"name", "status",
  /// "reason"}], "graph": string | null}`.
  pub fn to_json(&self) -> String {
    let mut out = format!(
      "{{\"path\":{},\"ok\":{},\"counts\":{{",
      json_string(&self.dir.display().to_string()),
      !self.failed()
    );
    let statuses = [Status::Pass, Status::Warn, Status::Fail, Status::Skip];
    for (i, status) in statuses.into_iter().enumerate() {
      let sep = if i == 0 { "" } else { "," };
      let _ = write!(out, "{sep}\"{}\":{}", status.label(), self.count(status));
    }
    out.push_str("},\"checks\":[");
    for (i, check) in self.checks.iter().enumerate() {
      let sep = if i == 0 { "" } else { "," };
      let _ = write!(
        out,
        "{sep}{{\"name\":{},\"status\":\"{}\",\"reason\":{}}}",
        json_string(check.name),
        check.status.label(),
        json_string(&check.reason)
      );
    }
    let graph = self.graph.as_deref().map_or("null".into(), json_string);
    let _ = writeln!(out, "],\"graph\":{graph}}}");
    out
  }
}

/// Runs every check on the project in `dir`, on this machine.
pub fn doctor(dir: &Path, options: &Options) -> Report {
  run_checks(dir, options, &Host)
}

fn run_checks(dir: &Path, options: &Options, sys: &dyn System) -> Report {
  // Absolute, so the lookups that walk up to the workspace root (`Cargo.lock`, an inherited
  // `rust-version`) work from `--path .`.
  let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
  let dir = dir.as_path();
  let mut report = Report {
    dir: dir.to_path_buf(),
    checks: Vec::new(),
    graph: None,
  };
  let project = match Project::load(dir) {
    Ok(project) => project,
    Err(reason) => {
      report.checks.push(Check::fail("project", reason));
      return report;
    }
  };
  report.checks.push(Check::pass(
    "project",
    match project.package_name() {
      Some(name) => format!("package `{name}`"),
      None => "workspace manifest (no [package])".to_string(),
    },
  ));
  report.checks.extend([
    rustc::check(&project, sys),
    versions::check(&project),
    env::check(&project, sys),
    database::check(&project, sys),
    docker::check(&project, sys),
    port::check(&project, sys),
  ]);
  if let Some(format) = options.graph {
    let (check, graph) = graph::check(&project, sys, format);
    report.checks.push(check);
    report.graph = graph;
  }
  report
}

/// `s` as a JSON string literal.
fn json_string(s: &str) -> String {
  let mut out = String::with_capacity(s.len() + 2);
  out.push('"');
  for c in s.chars() {
    match c {
      '"' => out.push_str("\\\""),
      '\\' => out.push_str("\\\\"),
      '\n' => out.push_str("\\n"),
      '\r' => out.push_str("\\r"),
      '\t' => out.push_str("\\t"),
      c if u32::from(c) < 0x20 => {
        let _ = write!(out, "\\u{:04x}", u32::from(c));
      }
      c => out.push(c),
    }
  }
  out.push('"');
  out
}

#[cfg(test)]
mod tests {
  use super::*;
  use fake::FakeSystem;

  fn report() -> Report {
    Report {
      dir: PathBuf::from("/srv/app"),
      checks: vec![
        Check::pass("rustc", "rustc 1.99.0"),
        Check::warn("docker", "no \"docker\""),
        Check::fail("versions", "two"),
        Check::skip("database", "unset"),
      ],
      graph: None,
    }
  }

  #[test]
  fn json_strings_escape_quotes_and_controls() {
    assert_eq!(json_string("a\"b\\c\nd\u{1}"), r#""a\"b\\c\nd\u0001""#);
  }

  #[test]
  fn text_has_a_line_per_check_and_a_summary() {
    let want = "\
id-effect doctor: /srv/app
  pass  rustc     rustc 1.99.0
  warn  docker    no \"docker\"
  fail  versions  two
  skip  database  unset

1 passed, 1 warning, 1 failed, 1 skipped
";
    assert_eq!(report().to_text(), want);
    let mut with_graph = report();
    with_graph.graph = Some("layer graph: 0 layers, 0 edges, 0 missing\n".into());
    assert!(
      with_graph
        .to_text()
        .ends_with("skipped\n\nlayer graph: 0 layers, 0 edges, 0 missing\n")
    );
  }

  #[test]
  fn json_has_counts_checks_and_the_graph() {
    let want = concat!(
      r#"{"path":"/srv/app","ok":false,"counts":{"pass":1,"warn":1,"fail":1,"skip":1},"#,
      r#""checks":[{"name":"rustc","status":"pass","reason":"rustc 1.99.0"},"#,
      r#"{"name":"docker","status":"warn","reason":"no \"docker\""},"#,
      r#"{"name":"versions","status":"fail","reason":"two"},"#,
      r#"{"name":"database","status":"skip","reason":"unset"}],"graph":null}"#,
      "\n"
    );
    assert_eq!(report().to_json(), want);
  }

  #[test]
  fn failed_means_some_check_failed() {
    let mut report = report();
    assert!(report.failed());
    report.checks.retain(|c| c.status != Status::Fail);
    assert!(!report.failed());
    assert!(report.to_json().contains("\"ok\":true"));
  }

  #[test]
  fn a_directory_without_cargo_toml_fails_the_project_check_only() {
    let tmp = tempfile::tempdir().unwrap();
    let report = run_checks(tmp.path(), &Options::default(), &FakeSystem::default());
    assert_eq!(report.checks.len(), 1);
    assert_eq!(report.checks[0].name, "project");
    assert_eq!(report.checks[0].status, Status::Fail);
  }

  #[test]
  fn a_project_runs_every_check_and_the_graph_on_request() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
      tmp.path().join("Cargo.toml"),
      "[package]\nname = \"shop\"\n",
    )
    .unwrap();
    let names = |report: &Report| report.checks.iter().map(|c| c.name).collect::<Vec<_>>();
    let sys = FakeSystem::default();
    let report = run_checks(tmp.path(), &Options::default(), &sys);
    assert_eq!(
      names(&report),
      [
        "project", "rustc", "versions", "env", "database", "docker", "port"
      ]
    );
    assert_eq!(report.checks[0].reason, "package `shop`");
    let options = Options {
      graph: Some(GraphFormat::Text),
    };
    let report = run_checks(tmp.path(), &options, &sys);
    assert_eq!(names(&report).last(), Some(&"graph"));
  }
}
